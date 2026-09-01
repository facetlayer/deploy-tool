//! Instance-wide reads for the dashboard (`admin-read`).
//!
//! Unlike every other read here, these are not scoped to one project — the
//! dashboard's job is to answer "what is on this server", which no per-project
//! grant can do. The transport has already checked the caller against the
//! instance administration resource before any of this runs; see
//! `rpc.rs::Action::AdminRead` for why that is the right resource.

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value as Json;

use deploy_core::rpc::{
    DeploymentInfo, GetProjectParams, GetProjectResult, ListProjectsResult, ProjectSummary,
};

use crate::handlers::parse_params;
use crate::state::AppState;

/// Deployment history is unbounded, and the dashboard renders a table.
const DEFAULT_HISTORY_LIMIT: i64 = 50;
const MAX_HISTORY_LIMIT: i64 = 500;

pub fn list_projects(state: &AppState, _params: &Json) -> Result<Json> {
    let conn = state.db();
    let mut stmt = conn.prepare(
        "select p.project_name, p.created_at, b.resource_name
         from project p
         left join project_resource_binding b on b.project_name = p.project_name
         order by p.project_name",
    )?;
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let projects = rows
        .into_iter()
        .map(|(project_name, created_at, resource_name)| {
            summarize(&conn, project_name, created_at, resource_name)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(serde_json::to_value(ListProjectsResult { projects })?)
}

pub fn get_project(state: &AppState, params: &Json) -> Result<Json> {
    let params: GetProjectParams = parse_params(params)?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);

    let conn = state.db();
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "select p.project_name, p.created_at, b.resource_name
             from project p
             left join project_resource_binding b on b.project_name = p.project_name
             where p.project_name = ?",
            rusqlite::params![params.project_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((project_name, created_at, resource_name)) = row else {
        return Err(anyhow!("no such project: {}", params.project_name));
    };

    let project = summarize(&conn, project_name.clone(), created_at, resource_name)?;
    let active = project.active_deploy_name.clone();

    let mut stmt = conn.prepare(
        "select deploy_name, created_at, tags_json, authorized_by_key_id, authorized_by_key_name
         from deployment where project_name = ? order by created_at desc limit ?",
    )?;
    let deployments = stmt
        .query_map(rusqlite::params![project_name, limit], |row| {
            let deploy_name: String = row.get(0)?;
            let tags_json: Option<String> = row.get(2)?;
            Ok(DeploymentInfo {
                is_active: Some(&deploy_name) == active.as_ref(),
                deploy_name,
                created_at: row.get(1)?,
                // A tags column that will not parse is not worth failing the
                // whole listing over; the row is still worth showing.
                tags: tags_json.and_then(|json| serde_json::from_str(&json).ok()),
                authorized_by_key_id: row.get(3)?,
                authorized_by_key_name: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(serde_json::to_value(GetProjectResult {
        project,
        deployments,
    })?)
}

/// The per-project counters both methods report. Kept in one place so a row in
/// the list and the header on the detail page can never disagree.
fn summarize(
    conn: &Connection,
    project_name: String,
    created_at: String,
    resource_name: Option<String>,
) -> Result<ProjectSummary> {
    let active: Option<(String, String)> = conn
        .query_row(
            "select deploy_name, updated_at from active_deployment where project_name = ?",
            rusqlite::params![project_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let (deployment_count, last_deployed_at): (i64, Option<String>) = conn.query_row(
        "select count(*), max(created_at) from deployment where project_name = ?",
        rusqlite::params![project_name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    // `active_deployment` can name a deployment row that no longer exists, so
    // this is a lookup rather than a join off the row above.
    let active_authorized_by = match &active {
        Some((deploy_name, _)) => conn
            .query_row(
                "select authorized_by_key_name from deployment where deploy_name = ?",
                rusqlite::params![deploy_name],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten(),
        None => None,
    };

    Ok(ProjectSummary {
        project_name,
        created_at,
        resource_name,
        active_deploy_name: active.as_ref().map(|(name, _)| name.clone()),
        active_since: active.as_ref().map(|(_, since)| since.clone()),
        last_deployed_at,
        deployment_count,
        active_authorized_by,
    })
}
