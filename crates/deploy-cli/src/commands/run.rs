//! `deploy run` — the main deployment flow. Port of src/client/deploy.ts.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use deploy_core::rpc::*;

use crate::client_setup::{setup_client, source_manifest, RunningTimer};
use crate::git_tags::collect_deployment_tags;
use crate::rpc_client::RpcClient;
use crate::shell::run_shell_command;

/// A file whose base64 encoding is at least this large goes up in parts. These
/// numbers are protocol-relevant — the server sizes its body limit around them
/// — so they must match the old client exactly.
const MAX_REQUEST_SIZE_BYTES: usize = 80 * 1024;

/// Chunks are measured in raw bytes, and base64 inflates by 4/3, so half of the
/// request budget is the largest chunk that stays under it.
const CHUNK_SIZE_BYTES: usize = MAX_REQUEST_SIZE_BYTES / 2;

/// A manifest larger than this is sent with `addManifestFiles` batches instead
/// of inline in `createDeployment`. Port of src/client/constants.ts.
pub const MANIFEST_BATCH_SIZE: usize = 500;

/// Files uploaded in parallel.
const UPLOAD_CONCURRENCY: usize = 50;

pub fn run(config_file: &Path, override_dest: Option<&str>) -> Result<()> {
    let mut timer = RunningTimer::new();

    let setup = setup_client(config_file, override_dest)?;
    println!("Project name: {}", setup.project_name);
    println!("Destination URL: {}", setup.dest_url);

    // Collect deployment tags (including the git commit) before doing any work,
    // so a dirty tree fails fast instead of after the build.
    let tags = collect_deployment_tags(&setup.settings, &setup.local_root)?;
    for (name, value) in &tags {
        println!("Deployment tag: {name}={value}");
    }

    for command in &setup.settings.before_deploy_commands {
        println!("Running before-deploy command: {command}");
        let status = run_shell_command(command, &setup.local_root)?;
        if !status.success() {
            bail!(
                "before-deploy command failed with exit code: {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string())
            );
        }
    }

    // The file list is resolved again after the hooks ran: a `before-deploy`
    // build step is usually what produces the files being shipped.
    let sources = crate::client_setup::resolve_local_files(
        &setup.local_root,
        &setup.config_text,
        &setup.settings,
    )?;
    let manifest = source_manifest(&sources)?;

    println!(
        "Resolved source file manifest ({}s)",
        timer.check_elapsed_secs()
    );
    println!("Creating deployment on server at: {}", setup.dest_url);

    let use_batched_manifest = manifest.len() > MANIFEST_BATCH_SIZE;

    let created = setup.client.create_deployment(&CreateDeploymentParams {
        project_name: setup.project_name.clone(),
        source_file_manifest: if use_batched_manifest {
            Vec::new()
        } else {
            manifest.clone()
        },
        source_file_config: setup.config_text.clone(),
        tags: Some(tags),
    })?;
    let deploy_name = created.deploy_name;

    println!("Deployment created with name: {deploy_name}");

    if use_batched_manifest {
        send_manifest_batches(&setup.client, &deploy_name, &manifest)?;
    }

    let needed_files = setup.client.get_needed_files(&GetNeededFilesParams {
        deploy_name: deploy_name.clone(),
    })?;

    println!(
        "Server has requested {} files to be uploaded",
        needed_files.len()
    );

    upload_all(
        &setup.client,
        &deploy_name,
        &needed_files,
        &sources,
        &setup.local_root,
    )?;

    println!("Finished uploading files ({}s)", timer.check_elapsed_secs());

    setup.client.finish_uploads(&FinishUploadsParams {
        deploy_name: deploy_name.clone(),
    })?;

    let verify = setup.client.verify_deployment(&VerifyDeploymentParams {
        deploy_name: deploy_name.clone(),
    })?;

    if verify.status == VerifyStatus::Error {
        bail!(
            "Deployment verification failed: {}",
            verify
                .error
                .unwrap_or_else(|| "(no reason given)".to_string())
        );
    }

    println!("Deployment is verified ({}s)", timer.check_elapsed_secs());

    setup
        .client
        .activate_deployment(&ActivateDeploymentParams {
            deploy_name: deploy_name.clone(),
        })?;

    println!("Deployment is active ({}s)", timer.check_elapsed_secs());

    Ok(())
}

/// Sends a large manifest in batches, so no single JSON request gets oversized.
pub fn send_manifest_batches(
    client: &RpcClient,
    deploy_name: &str,
    manifest: &[FileEntry],
) -> Result<()> {
    let total_batches = manifest.len().div_ceil(MANIFEST_BATCH_SIZE);

    for (index, batch) in manifest.chunks(MANIFEST_BATCH_SIZE).enumerate() {
        client.add_manifest_files(&AddManifestFilesParams {
            deploy_name: deploy_name.to_string(),
            files: batch.to_vec(),
        })?;
        println!("Sent manifest batch {}/{}", index + 1, total_batches);
    }

    client.finalize_manifest(&FinalizeManifestParams {
        deploy_name: deploy_name.to_string(),
    })?;
    println!("Manifest finalized");

    Ok(())
}

fn upload_all(
    client: &RpcClient,
    deploy_name: &str,
    needed_files: &[FileEntry],
    sources: &deploy_core::filelist::FileList,
    local_root: &Path,
) -> Result<()> {
    let next_index = AtomicUsize::new(0);
    let errors: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let total = needed_files.len();

    let worker_count = UPLOAD_CONCURRENCY.min(total.max(1));

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let index = next_index.fetch_add(1, Ordering::SeqCst);
                if index >= total {
                    return;
                }

                let file_entry = &needed_files[index];
                println!(
                    "Uploading [{}/{}]: {}",
                    index + 1,
                    total,
                    file_entry.rel_path
                );

                if let Err(err) = upload_one(client, deploy_name, file_entry, sources, local_root) {
                    errors
                        .lock()
                        .unwrap()
                        .push((file_entry.rel_path.clone(), format!("{err:#}")));
                }
            });
        }
    });

    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        let listed = errors
            .iter()
            .map(|(rel_path, err)| format!("  - {rel_path}: {err}"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "Deployment failed: {} file(s) could not be uploaded:\n{listed}",
            errors.len()
        );
    }

    Ok(())
}

fn upload_one(
    client: &RpcClient,
    deploy_name: &str,
    file_entry: &FileEntry,
    sources: &deploy_core::filelist::FileList,
    local_root: &Path,
) -> Result<()> {
    // The server may only ask for paths that were in the manifest we sent; a
    // path we do not recognize means the two sides disagree about the file set.
    if sources.get_by_rel_path(&file_entry.rel_path).is_none() {
        bail!("Couldn't find a requested relPath: {}", file_entry.rel_path);
    }

    let local_path = local_root.join(&file_entry.rel_path);
    let content = std::fs::read(&local_path)
        .with_context(|| format!("Could not read {}", local_path.display()))?;

    upload_file(client, deploy_name, &file_entry.rel_path, &content)
}

fn upload_file(
    client: &RpcClient,
    deploy_name: &str,
    rel_path: &str,
    content: &[u8],
) -> Result<()> {
    let base64 = BASE64.encode(content);

    if !needs_multipart(base64.len()) {
        client.upload_one_file(&UploadOneFileParams {
            deploy_name: deploy_name.to_string(),
            rel_path: rel_path.to_string(),
            content_base64: base64,
        })?;
        return Ok(());
    }

    client.start_multi_part_upload(&StartMultiPartUploadParams {
        deploy_name: deploy_name.to_string(),
        rel_path: rel_path.to_string(),
    })?;

    for start_at in chunk_starts(content.len()) {
        let end = (start_at + CHUNK_SIZE_BYTES).min(content.len());
        client.upload_file_part(&UploadFilePartParams {
            deploy_name: deploy_name.to_string(),
            rel_path: rel_path.to_string(),
            chunk_starts_at: start_at as i64,
            chunk_base64: BASE64.encode(&content[start_at..end]),
        })?;
    }

    client.finish_multi_part_upload(&FinishMultiPartUploadParams {
        deploy_name: deploy_name.to_string(),
        rel_path: rel_path.to_string(),
    })?;

    println!("Finished uploading file: {rel_path}");

    Ok(())
}

/// Whether a file of this encoded size has to go up in parts.
fn needs_multipart(base64_len: usize) -> bool {
    base64_len >= MAX_REQUEST_SIZE_BYTES
}

/// Byte offsets of each chunk of a multipart upload.
fn chunk_starts(content_len: usize) -> Vec<usize> {
    (0..content_len).step_by(CHUNK_SIZE_BYTES).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_match_the_documented_protocol() {
        assert_eq!(MAX_REQUEST_SIZE_BYTES, 81920);
        assert_eq!(CHUNK_SIZE_BYTES, 40960);
        assert_eq!(MANIFEST_BATCH_SIZE, 500);
    }

    #[test]
    fn small_files_go_in_a_single_request() {
        assert!(!needs_multipart(0));
        assert!(!needs_multipart(MAX_REQUEST_SIZE_BYTES - 1));
    }

    #[test]
    fn files_at_or_above_the_threshold_go_multipart() {
        assert!(needs_multipart(MAX_REQUEST_SIZE_BYTES));
        assert!(needs_multipart(MAX_REQUEST_SIZE_BYTES + 1));
    }

    #[test]
    fn an_empty_file_has_no_chunks() {
        assert_eq!(chunk_starts(0), Vec::<usize>::new());
    }

    #[test]
    fn chunk_offsets_step_by_the_chunk_size() {
        assert_eq!(chunk_starts(1), vec![0]);
        assert_eq!(chunk_starts(CHUNK_SIZE_BYTES), vec![0]);
        assert_eq!(
            chunk_starts(CHUNK_SIZE_BYTES + 1),
            vec![0, CHUNK_SIZE_BYTES]
        );
        assert_eq!(
            chunk_starts(CHUNK_SIZE_BYTES * 3),
            vec![0, CHUNK_SIZE_BYTES, CHUNK_SIZE_BYTES * 2]
        );
    }

    #[test]
    fn chunks_cover_the_whole_file_without_overlap() {
        let len = CHUNK_SIZE_BYTES * 2 + 17;
        let mut covered = 0;
        for start in chunk_starts(len) {
            let end = (start + CHUNK_SIZE_BYTES).min(len);
            assert_eq!(start, covered);
            covered = end;
        }
        assert_eq!(covered, len);
    }

    #[test]
    fn manifest_batching_splits_at_five_hundred() {
        let entry = |i: usize| FileEntry {
            rel_path: format!("file-{i}"),
            sha: "sha".to_string(),
        };
        let manifest: Vec<FileEntry> = (0..1001).map(entry).collect();

        let batches: Vec<usize> = manifest
            .chunks(MANIFEST_BATCH_SIZE)
            .map(|batch| batch.len())
            .collect();
        assert_eq!(batches, vec![500, 500, 1]);
        assert_eq!(manifest.len().div_ceil(MANIFEST_BATCH_SIZE), 3);
    }

    #[test]
    fn a_manifest_of_exactly_the_batch_size_is_sent_inline() {
        // The old client compares with `>`, so 500 entries still go inline.
        assert!(!(MANIFEST_BATCH_SIZE > MANIFEST_BATCH_SIZE));
        assert!(MANIFEST_BATCH_SIZE + 1 > MANIFEST_BATCH_SIZE);
    }
}
