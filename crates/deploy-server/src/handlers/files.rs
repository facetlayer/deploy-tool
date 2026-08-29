//! File-oriented methods: needed-file discovery, verification, preview, download.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{anyhow, Result};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value as Json;

use deploy_core::hash::get_file_hash;
use deploy_core::rpc::{
    DownloadFileParams, DownloadFileResult, FileEntry, GetNeededFilesParams,
    PreviewByDeployNameParams, PreviewDeploymentParams, PreviewDeploymentResult,
    VerifyDeploymentParams, VerifyDeploymentResult, VerifyStatus,
};

use crate::db;
use crate::handlers::cleanup::database_cleanup;
use crate::handlers::parse_params;
use crate::manifest::{manifest_file_list, parse_manifest_json};
use crate::paths::{get_active_deployment_dir, get_safe_path_in_dir};
use crate::preserve::find_leftovers_respecting_preserve;
use crate::state::AppState;

/// Verification hashes every file in the deployment; the work is IO-bound, so
/// it runs across a small fixed pool rather than one file at a time.
const HASH_CONCURRENCY: usize = 20;
const PROGRESS_LOG_INTERVAL_FILES: usize = 500;

pub struct DeploymentRow {
    pub deploy_dir: String,
    pub project_name: String,
    pub source_config_file: Option<String>,
    pub manifest_json: Option<String>,
}

pub fn load_deployment(conn: &Connection, deploy_name: &str) -> Result<Option<DeploymentRow>> {
    Ok(conn
        .query_row(
            "select deploy_dir, project_name, source_config_file, manifest_json
             from deployment where deploy_name = ?",
            [deploy_name],
            |row| {
                Ok(DeploymentRow {
                    deploy_dir: row.get(0)?,
                    project_name: row.get(1)?,
                    source_config_file: row.get(2)?,
                    manifest_json: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// An unreadable file counts as absent here: the point is only "is what is on
/// disk already the content we want", and anything else means upload it again.
fn hash_or_none(path: &std::path::Path) -> Option<String> {
    get_file_hash(path).unwrap_or(None)
}

/// Port of `src/server/getNeededFiles.ts`.
pub fn get_needed_files(state: &AppState, params: &Json) -> Result<Json> {
    let params: GetNeededFilesParams = parse_params(params)?;
    let deploy_name = params.deploy_name;

    let conn = state.db();
    let deployment = load_deployment(&conn, &deploy_name)?
        .ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?;

    let manifest = parse_manifest_json(deployment.manifest_json.as_deref());
    let deploy_dir = db::get_deployments_dir(&conn)?.join(&deployment.deploy_dir);

    let mut needed: Vec<FileEntry> = Vec::new();

    for file in &manifest {
        let target_path = get_safe_path_in_dir(&deploy_dir, &file.rel_path)?;

        if hash_or_none(&target_path).as_deref() != Some(file.sha.as_str()) {
            needed.push(file.clone());

            // Tracks what still has to arrive before the deployment is
            // complete; verifyDeployment refuses while any row remains. A
            // duplicate row from a repeated call is harmless.
            let _ = conn.execute(
                "insert into deployment_needed_file (deploy_name, rel_path, sha, created_at)
                 values (?, ?, ?, ?)",
                rusqlite::params![deploy_name, file.rel_path, file.sha, db::now_iso()],
            );
        }
    }

    Ok(serde_json::to_value(needed)?)
}

/// Port of `src/server/verifyDeployment.ts`.
pub fn verify_deployment(state: &AppState, params: &Json) -> Result<Json> {
    let params: VerifyDeploymentParams = parse_params(params)?;
    let deploy_name = params.deploy_name;

    let conn = state.db();
    let deployment = load_deployment(&conn, &deploy_name)?
        .ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?;

    let manifest = parse_manifest_json(deployment.manifest_json.as_deref());
    let deploy_dir = db::get_deployments_dir(&conn)?.join(&deployment.deploy_dir);

    let missing_file_count: i64 = conn.query_row(
        "select count(*) from deployment_needed_file where deploy_name = ?",
        [&deploy_name],
        |row| row.get(0),
    )?;

    if missing_file_count > 0 {
        println!(
            "Deployment verification failed: {} - {} files are missing",
            deploy_name, missing_file_count
        );
        return verify_error(format!(
            "Incomplete deployment: {} files are missing",
            missing_file_count
        ));
    }

    let total = manifest.len();
    println!("Verifying deployment {}: hashing {} files...", deploy_name, total);

    // Resolve every target path up front, so the parallel hashing below needs
    // no database access.
    let targets: Vec<(String, String, PathBuf)> = manifest
        .iter()
        .map(|file| {
            get_safe_path_in_dir(&deploy_dir, &file.rel_path)
                .map(|path| (file.rel_path.clone(), file.sha.clone(), path))
        })
        .collect::<Result<Vec<_>>>()?;

    let next_index = AtomicUsize::new(0);
    let verified = AtomicUsize::new(0);
    let mut results: Vec<Option<String>> = vec![None; targets.len()];

    {
        let results_slot = std::sync::Mutex::new(&mut results);
        let worker_count = HASH_CONCURRENCY.min(targets.len().max(1));

        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                scope.spawn(|| loop {
                    let index = next_index.fetch_add(1, Ordering::SeqCst);
                    if index >= targets.len() {
                        break;
                    }

                    let (rel_path, expected_sha, local_path) = &targets[index];

                    let outcome = match hash_or_none(local_path) {
                        None => {
                            println!(
                                "Deployment verification failed: {} - file is missing: {}",
                                deploy_name, rel_path
                            );
                            Some(format!("Incomplete deployment: file is missing: {}", rel_path))
                        }
                        Some(sha) if &sha != expected_sha => {
                            println!(
                                "Deployment verification failed: {} - file has wrong contents: {}",
                                deploy_name, rel_path
                            );
                            Some(format!(
                                "Incomplete deployment: file has wrong contents: {}",
                                rel_path
                            ))
                        }
                        Some(_) => {
                            let count = verified.fetch_add(1, Ordering::SeqCst) + 1;
                            if count % PROGRESS_LOG_INTERVAL_FILES == 0 {
                                println!(
                                    "Verifying deployment {}: {}/{} files verified...",
                                    deploy_name, count, total
                                );
                            }
                            None
                        }
                    };

                    if let Some(error) = outcome {
                        let mut slot = results_slot.lock().unwrap();
                        slot[index] = Some(error);
                    }
                });
            }
        });
    }

    if let Some(first_error) = results.into_iter().flatten().next() {
        return verify_error(first_error);
    }

    println!(
        "Deployment verification complete: {} - all {} files verified",
        deploy_name, total
    );

    conn.execute(
        "delete from deployment_pending_multi_part_file_chunk where deploy_name = ?",
        [&deploy_name],
    )?;

    database_cleanup(&conn)?;

    Ok(serde_json::to_value(VerifyDeploymentResult {
        status: VerifyStatus::Success,
        error: None,
    })?)
}

/// Verification failures are reported in-band, not as a JSON-RPC error, so the
/// CLI can print the reason and stop before activating.
fn verify_error(message: String) -> Result<Json> {
    Ok(serde_json::to_value(VerifyDeploymentResult {
        status: VerifyStatus::Error,
        error: Some(message),
    })?)
}

/// Port of `src/server/previewDeployment.ts`.
pub fn preview_deployment(
    state: &AppState,
    project_name: &str,
    source_file_manifest: &[FileEntry],
    source_file_config: &str,
) -> Result<Json> {
    let deploy_dir = {
        let conn = state.db();
        get_active_deployment_dir(&conn, project_name)?
    };

    let deploy_dir = match deploy_dir {
        Some(dir) => dir,
        None => {
            // No active deployment: everything is new, and there is nothing on
            // the server that could be deleted.
            return Ok(serde_json::to_value(PreviewDeploymentResult {
                files_to_upload: source_file_manifest.to_vec(),
                files_to_delete: Vec::new(),
            })?);
        }
    };

    let files_to_upload: Vec<FileEntry> = source_file_manifest
        .iter()
        .filter(|file| {
            hash_or_none(&deploy_dir.join(&file.rel_path)).as_deref() != Some(file.sha.as_str())
        })
        .cloned()
        .collect();

    let incoming = manifest_file_list(source_file_manifest);
    let scan = find_leftovers_respecting_preserve(&deploy_dir, &incoming, source_file_config)?;

    Ok(serde_json::to_value(PreviewDeploymentResult {
        files_to_upload,
        files_to_delete: scan.leftovers.rel_paths(),
    })?)
}

pub fn preview_deployment_method(state: &AppState, params: &Json) -> Result<Json> {
    let params: PreviewDeploymentParams = parse_params(params)?;
    preview_deployment(
        state,
        &params.project_name,
        &params.source_file_manifest,
        &params.source_file_config,
    )
}

pub fn preview_by_deploy_name(state: &AppState, params: &Json) -> Result<Json> {
    let params: PreviewByDeployNameParams = parse_params(params)?;
    let deploy_name = params.deploy_name;

    let deployment = {
        let conn = state.db();
        load_deployment(&conn, &deploy_name)?
            .ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?
    };

    let manifest = parse_manifest_json(deployment.manifest_json.as_deref());

    preview_deployment(
        state,
        &deployment.project_name,
        &manifest,
        deployment.source_config_file.as_deref().unwrap_or(""),
    )
}

/// Port of `src/server/downloadFile.ts`.
pub fn download_file(state: &AppState, params: &Json) -> Result<Json> {
    let params: DownloadFileParams = parse_params(params)?;

    let deploy_dir = {
        let conn = state.db();
        get_active_deployment_dir(&conn, &params.project_name)?
    };

    let deploy_dir = deploy_dir.ok_or_else(|| {
        anyhow!(
            "No active deployment found for project: {}",
            params.project_name
        )
    })?;

    let full_path = get_safe_path_in_dir(&deploy_dir, &params.rel_path)?;
    let content = std::fs::read(&full_path)?;

    Ok(serde_json::to_value(DownloadFileResult {
        content_base64: base64::engine::general_purpose::STANDARD.encode(&content),
        rel_path: params.rel_path,
    })?)
}
