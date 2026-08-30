//! Deployment lifecycle: project registration, create, manifest batching,
//! listing, rollback.

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value as Json};

use deploy_core::config::parse_create_settings;
use deploy_core::rpc::{
    AddManifestFilesParams, CreateDeploymentParams, CreateProjectOutcome, CreateProjectParams,
    CreateProjectResult, DeploymentCreatedEvent, DeploymentInfo, FinalizeManifestParams,
    ListDeploymentsParams, ListDeploymentsResult, RollbackParams,
};

use crate::db;
use crate::handlers::cleanup::database_cleanup;
use crate::handlers::parse_params;
use crate::handlers::tags::parse_tags_json;
use crate::manifest::{manifest_to_json, setup_empty_directories};
use crate::state::{AppState, AuthorizedKey};

const DEFAULT_LIST_LIMIT: i64 = 10;

/// R1: register a project and bind it to an auth-center resource.
///
/// Authorization for this method lives in the transport (it is checked against
/// this instance's administration resource, per D2); everything here is the
/// storage side of that decision.
pub fn create_project(
    state: &AppState,
    params: &Json,
    authorized_by: Option<&AuthorizedKey>,
) -> Result<Json> {
    let params: CreateProjectParams = parse_params(params)?;
    let project_name = params.project_name.trim().to_string();
    let resource_name = params.resource_name.trim().to_string();

    if project_name.is_empty() {
        return Err(anyhow!("projectName is required"));
    }
    // An empty resource would register a project that no key can ever satisfy,
    // and would read as "unbound" to createDeployment.
    if resource_name.is_empty() {
        return Err(anyhow!("resourceName is required"));
    }
    // A scope is `<resource>:<action>` and auth-center rejects a third segment,
    // so a colon here would produce a scope no key could ever be granted. Catch
    // it at registration rather than as a mass denial at the next deploy.
    if resource_name.contains(':') {
        return Err(anyhow!(
            "resourceName must not contain ':' (got {resource_name:?}); \
             a scope is <resource>:<action>, so put any grouping in the name \
             itself, e.g. \"do2-deploy\" rather than \"deploy:do2\""
        ));
    }

    let conn = state.db();

    let existing: Option<String> = conn
        .query_row(
            "select resource_name from project_resource_binding where project_name = ?",
            [&project_name],
            |row| row.get(0),
        )
        .optional()?;

    let now = db::now_iso();
    let (changed_by_key_id, changed_by_key_name) = match authorized_by {
        Some(key) => (Some(key.key_id.clone()), key.key_name.clone()),
        None => (None, None),
    };

    let registered: bool = conn
        .query_row(
            "select 1 from project where project_name = ?",
            [&project_name],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !registered {
        conn.execute(
            "insert into project (project_name, created_at) values (?, ?)",
            rusqlite::params![project_name, now],
        )?;
    }

    let (outcome, previous_resource_name) = match existing {
        None => {
            bind_resource(
                &conn,
                &project_name,
                &resource_name,
                &now,
                (&changed_by_key_id, &changed_by_key_name),
            )?;
            (CreateProjectOutcome::Created, None)
        }
        Some(bound) if bound == resource_name => {
            // Re-running the same registration is a no-op, and deliberately
            // writes no history row: nothing changed.
            (CreateProjectOutcome::Unchanged, Some(bound))
        }
        Some(bound) => {
            // Repointing a project at a different resource hands its deploy
            // rights to a different set of keys, so it is never implicit.
            if !params.rebind {
                return Err(anyhow!(
                    "Project '{}' is already bound to resource '{}'. Pass rebind to repoint it to '{}'.",
                    project_name,
                    bound,
                    resource_name
                ));
            }
            bind_resource(
                &conn,
                &project_name,
                &resource_name,
                &now,
                (&changed_by_key_id, &changed_by_key_name),
            )?;
            (CreateProjectOutcome::Rebound, Some(bound))
        }
    };

    if outcome != CreateProjectOutcome::Unchanged {
        conn.execute(
            "insert into project_resource_binding_history
                (project_name, previous_resource_name, resource_name, changed_at,
                 changed_by_key_id, changed_by_key_name)
             values (?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                project_name,
                previous_resource_name,
                resource_name,
                now,
                changed_by_key_id,
                changed_by_key_name
            ],
        )?;
    }

    Ok(serde_json::to_value(CreateProjectResult {
        project_name,
        resource_name,
        outcome,
        previous_resource_name,
    })?)
}

/// Writes the current binding. The history row is written by the caller, which
/// knows whether anything actually changed.
fn bind_resource(
    conn: &Connection,
    project_name: &str,
    resource_name: &str,
    now: &str,
    changed_by: (&Option<String>, &Option<String>),
) -> Result<()> {
    conn.execute(
        "insert into project_resource_binding
            (project_name, resource_name, bound_at, bound_by_key_id, bound_by_key_name)
         values (?, ?, ?, ?, ?)
         on conflict(project_name) do update set
            resource_name = excluded.resource_name,
            bound_at = excluded.bound_at,
            bound_by_key_id = excluded.bound_by_key_id,
            bound_by_key_name = excluded.bound_by_key_name",
        rusqlite::params![project_name, resource_name, now, changed_by.0, changed_by.1],
    )?;
    Ok(())
}

/// R1: a deploy may only target a project that is registered and bound to a
/// resource. There is no implicit-create path: an unbound project is one no key
/// can be checked against, which R2 says must be a denial rather than a guess.
fn ensure_project_registered(conn: &Connection, project_name: &str) -> Result<()> {
    let bound: Option<String> = conn
        .query_row(
            "select resource_name from project_resource_binding where project_name = ?",
            [project_name],
            |row| row.get(0),
        )
        .optional()?;

    match bound {
        Some(_) => Ok(()),
        None => Err(anyhow!(
            "Project '{}' is not registered on this server. \
             Run: deploy create-project {} --resource <resourceName>",
            project_name,
            project_name
        )),
    }
}

/// Port of `src/server/createDeployment.ts`, plus R1 registration and R7
/// attribution.
pub fn create_deployment(
    state: &AppState,
    params: &Json,
    authorized_by: Option<&AuthorizedKey>,
) -> Result<Json> {
    let params: CreateDeploymentParams = parse_params(params)?;
    let project_name = params.project_name;
    let source_file_config = params.source_file_config;
    let source_file_manifest = params.source_file_manifest;

    let conn = state.db();
    database_cleanup(&conn)?;

    ensure_project_registered(&conn, &project_name)?;

    let deploy_id = db::take_next_deploy_id(&conn)?;
    let deploy_name = format!("{}-{}", project_name, deploy_id);

    let settings = parse_create_settings(&source_file_config);

    // update-in-place deploys write into a directory named for the project, so
    // successive deploys land on top of each other instead of getting their own
    // directory to be swapped in.
    let deploy_dir = if settings.is_update_in_place {
        project_name.clone()
    } else {
        deploy_name.clone()
    };
    let full_deploy_dir = db::get_deployments_dir(&conn)?.join(&deploy_dir);

    let dynamic_routes_json: Option<String> = if settings.dynamic_routes.is_empty() {
        None
    } else {
        let routes: Vec<Json> = settings
            .dynamic_routes
            .iter()
            .map(|route| {
                let mut obj = serde_json::Map::new();
                obj.insert("pattern".into(), json!(route.pattern));
                obj.insert("file".into(), json!(route.file));
                if let Some(source) = &route.metadata_source {
                    obj.insert("metadataSource".into(), json!(source));
                }
                if let Some(ttl) = route.metadata_cache_ttl {
                    obj.insert("metadataCacheTtl".into(), json!(ttl));
                }
                Json::Object(obj)
            })
            .collect();
        Some(serde_json::to_string(&routes)?)
    };

    let tags_json: Option<String> = match &params.tags {
        Some(tags) if !tags.is_empty() => Some(serde_json::to_string(tags)?),
        _ => None,
    };

    let (authorized_by_key_id, authorized_by_key_name) = match authorized_by {
        Some(key) => (Some(key.key_id.clone()), key.key_name.clone()),
        None => (None, None),
    };

    conn.execute(
        "insert into deployment (deploy_name, deploy_dir, project_name, web_static_dir,
            dynamic_routes_json, tags_json, created_at, source_config_file, manifest_json,
            authorized_by_key_id, authorized_by_key_name)
         values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            deploy_name,
            deploy_dir,
            project_name,
            settings.web_static_dir,
            dynamic_routes_json,
            tags_json,
            db::now_iso(),
            source_file_config,
            manifest_to_json(&source_file_manifest)?,
            authorized_by_key_id,
            authorized_by_key_name,
        ],
    )?;

    println!(
        "Setting up new deployment at:  {}",
        full_deploy_dir.display()
    );
    let _ = std::fs::create_dir(&full_deploy_dir);

    // Only lay out directories when the manifest came inline. A large deploy
    // sends it in batches and creates its directories in finalizeManifest.
    if !source_file_manifest.is_empty() {
        setup_empty_directories(&full_deploy_dir, &source_file_manifest)?;
    }

    println!("Deployment created: {}", deploy_name);

    Ok(serde_json::to_value(DeploymentCreatedEvent::new(
        deploy_name,
    ))?)
}

pub fn add_manifest_files(state: &AppState, params: &Json) -> Result<Json> {
    let params: AddManifestFilesParams = parse_params(params)?;
    let deploy_name = params.deploy_name;

    let conn = state.db();
    let manifest_json: Option<Option<String>> = conn
        .query_row(
            "select manifest_json from deployment where deploy_name = ?",
            [&deploy_name],
            |row| row.get(0),
        )
        .optional()?;

    let manifest_json =
        manifest_json.ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?;

    let mut existing = crate::manifest::parse_manifest_json(manifest_json.as_deref());
    existing.extend(params.files);

    conn.execute(
        "update deployment set manifest_json = ? where deploy_name = ?",
        rusqlite::params![manifest_to_json(&existing)?, deploy_name],
    )?;

    Ok(Json::Null)
}

/// Port of `src/server/finalizeManifest.ts`.
pub fn finalize_manifest(state: &AppState, params: &Json) -> Result<Json> {
    let params: FinalizeManifestParams = parse_params(params)?;
    let deploy_name = params.deploy_name;

    let conn = state.db();
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "select deploy_dir, manifest_json from deployment where deploy_name = ?",
            [&deploy_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let (deploy_dir, manifest_json) =
        row.ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?;

    let manifest = crate::manifest::parse_manifest_json(manifest_json.as_deref());
    let full_deploy_dir = db::get_deployments_dir(&conn)?.join(deploy_dir);
    setup_empty_directories(&full_deploy_dir, &manifest)?;

    println!(
        "Manifest finalized for {} with {} files",
        deploy_name,
        manifest.len()
    );

    Ok(Json::Null)
}

pub fn list_deployments(state: &AppState, params: &Json) -> Result<Json> {
    let params: ListDeploymentsParams = parse_params(params)?;
    let project_name = params.project_name;
    let limit = params.limit.unwrap_or(DEFAULT_LIST_LIMIT);

    let conn = state.db();

    type Row = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> = {
        let mut stmt = conn.prepare(
            "select deploy_name, created_at, tags_json, authorized_by_key_id,
                    authorized_by_key_name
             from deployment where project_name = ? order by created_at desc limit ?",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_name, limit], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let active_deploy_name: Option<String> = conn
        .query_row(
            "select deploy_name from active_deployment where project_name = ?",
            [&project_name],
            |row| row.get(0),
        )
        .optional()?;

    let deployments: Vec<DeploymentInfo> = rows
        .into_iter()
        .map(
            |(deploy_name, created_at, tags_json, key_id, key_name)| DeploymentInfo {
                is_active: Some(&deploy_name) == active_deploy_name.as_ref(),
                deploy_name,
                created_at,
                // Always present, even when empty: the old server emitted the
                // field unconditionally and an old CLI reads it directly.
                tags: Some(parse_tags_json(tags_json.as_deref())),
                authorized_by_key_id: key_id,
                authorized_by_key_name: key_name,
            },
        )
        .collect();

    Ok(serde_json::to_value(ListDeploymentsResult {
        deployments,
        active_deploy_name,
    })?)
}

pub fn rollback(state: &AppState, params: &Json) -> Result<Json> {
    let params: RollbackParams = parse_params(params)?;

    {
        // The transport authorized against projectName, so the deployment being
        // rolled back has to actually belong to that project.
        let conn = state.db();
        let found: Option<String> = conn
            .query_row(
                "select deploy_name from deployment where deploy_name = ? and project_name = ?",
                rusqlite::params![params.deploy_name, params.project_name],
                |row| row.get(0),
            )
            .optional()?;

        if found.is_none() {
            return Err(anyhow!(
                "Deployment '{}' not found for project '{}'",
                params.deploy_name,
                params.project_name
            ));
        }
    }

    crate::handlers::activate::activate_deployment(
        state,
        &json!({ "deployName": params.deploy_name }),
    )
}
