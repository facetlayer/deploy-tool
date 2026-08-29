//! File upload methods, including the multi-part upload flow.

use std::io::Write;

use anyhow::{anyhow, Result};
use base64::Engine;
use rusqlite::OptionalExtension;
use serde_json::Value as Json;

use deploy_core::rpc::{
    FinishMultiPartUploadParams, FinishUploadsParams, UploadFilePartParams, UploadOneFileParams,
};

use crate::db;
use crate::handlers::parse_params;
use crate::manifest::{manifest_file_list, parse_manifest_json};
use crate::paths::get_path_in_deployment_dir;
use crate::preserve::{find_leftovers_respecting_preserve, prune_stale_preserved_files};
use crate::state::AppState;

/// Node's `Buffer.from(str, 'base64')` is lenient about padding and stray
/// whitespace. An old TypeScript CLI's chunks have to keep decoding here, so
/// this mirrors that leniency rather than using the strict decoder.
fn decode_base64(text: &str) -> Result<Vec<u8>> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(cleaned.trim_end_matches('='))
        })
        .map_err(|err| anyhow!("invalid base64 content: {}", err))
}

pub fn upload_one_file(state: &AppState, params: &Json) -> Result<Json> {
    let params: UploadOneFileParams = parse_params(params)?;

    let local_path = {
        let conn = state.db();
        get_path_in_deployment_dir(&conn, &params.deploy_name, &params.rel_path)?
    };

    let contents = decode_base64(&params.content_base64)?;
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&local_path, contents)?;

    let conn = state.db();
    conn.execute(
        "delete from deployment_needed_file where deploy_name = ? and rel_path = ?",
        rusqlite::params![params.deploy_name, params.rel_path],
    )?;

    Ok(Json::Null)
}

pub fn upload_file_part(state: &AppState, params: &Json) -> Result<Json> {
    let params: UploadFilePartParams = parse_params(params)?;

    let conn = state.db();
    conn.execute(
        "insert into deployment_pending_multi_part_file_chunk
            (deploy_name, rel_path, chunk_start_at, chunk_base64, created_at)
         values (?, ?, ?, ?, ?)",
        rusqlite::params![
            params.deploy_name,
            params.rel_path,
            params.chunk_starts_at,
            params.chunk_base64,
            db::now_iso()
        ],
    )?;

    Ok(Json::Null)
}

pub fn finish_multipart_upload(state: &AppState, params: &Json) -> Result<Json> {
    let params: FinishMultiPartUploadParams = parse_params(params)?;
    let deploy_name = params.deploy_name;
    let rel_path = params.rel_path;

    let (local_path, chunks) = {
        let conn = state.db();
        let local_path = get_path_in_deployment_dir(&conn, &deploy_name, &rel_path)?;

        let mut stmt = conn.prepare(
            "select chunk_start_at, chunk_base64 from deployment_pending_multi_part_file_chunk
             where deploy_name = ? and rel_path = ?",
        )?;
        let rows = stmt.query_map(rusqlite::params![&deploy_name, &rel_path], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut chunks = rows.collect::<Result<Vec<_>, _>>()?;
        // Chunks arrive in whatever order the client's requests completed.
        chunks.sort_by_key(|(start, _)| *start);
        (local_path, chunks)
    };

    // Truncate any previous copy before appending, so a retried upload does not
    // concatenate onto the old contents.
    let _ = std::fs::remove_file(&local_path);
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&local_path)?;
    for (_, chunk_base64) in &chunks {
        file.write_all(&decode_base64(chunk_base64)?)?;
    }
    file.flush()?;
    drop(file);

    let conn = state.db();
    conn.execute(
        "delete from deployment_pending_multi_part_file_chunk where deploy_name = ? and rel_path = ?",
        rusqlite::params![&deploy_name, &rel_path],
    )?;
    conn.execute(
        "delete from deployment_needed_file where deploy_name = ? and rel_path = ?",
        rusqlite::params![&deploy_name, &rel_path],
    )?;

    Ok(Json::Null)
}

/// Port of `src/server/finishUploads.ts`.
pub fn finish_uploads(state: &AppState, params: &Json) -> Result<Json> {
    let params: FinishUploadsParams = parse_params(params)?;
    let deploy_name = params.deploy_name;

    let (deploy_dir, source_config, manifest) = {
        let conn = state.db();

        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "select manifest_json, source_config_file from deployment where deploy_name = ?",
                [&deploy_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (manifest_json, source_config) =
            row.ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?;

        // Without a manifest there is no record of what this deploy shipped, so
        // every file on the server would look orphaned. Refuse rather than
        // delete the deployment's contents.
        let manifest_json = manifest_json
            .ok_or_else(|| anyhow!("Manifest not found for deployment: {}", deploy_name))?;
        let manifest = parse_manifest_json(Some(&manifest_json));

        let deploy_dir = get_path_in_deployment_dir(&conn, &deploy_name, "")?;
        (deploy_dir, source_config.unwrap_or_default(), manifest)
    };

    let incoming = manifest_file_list(&manifest);

    // DANGEROUS: everything this reports is deleted below. See the hazard note
    // on deploy_core::filelist::find_leftover_files.
    let scan = find_leftovers_respecting_preserve(&deploy_dir, &incoming, &source_config)?;

    for file in scan.leftovers.list_all() {
        println!("  Deleting: {}", file.rel_path);
        if let Err(error) = std::fs::remove_file(&file.source_path) {
            eprintln!("Failed to delete {}: {}", file.rel_path, error);
        }
    }

    // Garbage-collect preserved files older than the configured max age, so a
    // preserve rule doesn't accumulate every hashed asset ever deployed.
    if !scan.preserve.patterns.is_empty() {
        if let Some(max_age_ms) = scan.preserve.max_age_ms {
            prune_stale_preserved_files(
                &deploy_dir,
                &incoming,
                &scan.parsed_rules,
                &scan.leftovers,
                max_age_ms,
            );
        }
    }

    Ok(Json::Null)
}
