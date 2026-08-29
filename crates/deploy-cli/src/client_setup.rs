//! Shared command setup. Port of src/client/clientSetup.ts and
//! src/client/fileList.ts.
//!
//! Every command starts the same way: read the config, work out the local root,
//! resolve the file list, run the security scan, then build an authenticated
//! RPC client for the destination.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use deploy_core::config::{self, ClientSettings};
use deploy_core::filelist::FileList;
use deploy_core::hash::get_file_hash;
use deploy_core::rpc::FileEntry;
use deploy_core::security;

use crate::api_key::find_api_key;
use crate::rpc_client::RpcClient;

pub struct ClientSetup {
    pub client: RpcClient,
    pub project_name: String,
    pub dest_url: String,
    pub local_dir: PathBuf,
    pub config_text: String,
    pub settings: ClientSettings,
    /// The local file list, already security-scanned.
    pub files: FileList,
}

/// Reads a config file, resolves its file list and returns a client pointed at
/// the destination.
///
/// The security scan runs here rather than in the deploy path, so that every
/// command that reads a config — not just `run` — refuses to operate on a file
/// set containing credentials.
pub fn setup_client(config_file: &Path, override_dest: Option<&str>) -> Result<ClientSetup> {
    let config_text = std::fs::read_to_string(config_file)
        .with_context(|| format!("Could not read config file: {}", config_file.display()))?;

    let settings = config::parse_client_settings(&config_text);
    let local_dir = config::resolve_local_dir(config_file, &settings, None)?;

    let project_name = settings.project_name.clone().ok_or_else(|| {
        anyhow!(
            "Config file {} has no project-name in its deploy-settings block",
            config_file.display()
        )
    })?;

    let dest_url = match override_dest {
        Some(dest) => dest.to_string(),
        None => settings.dest_url.clone().ok_or_else(|| {
            anyhow!(
                "Config file {} has no dest-url in its deploy-settings block; \
                 pass --override-dest to supply one",
                config_file.display()
            )
        })?,
    };

    let files = resolve_local_files(&local_dir, &config_text, &settings)?;

    let mut client = RpcClient::new(&dest_url);
    match find_api_key(settings.secrets_file.as_deref()) {
        Some(api_key) => client.set_api_key(api_key),
        None => {
            let sources = match &settings.secrets_file {
                Some(file) => format!(
                    "{file}, DEPLOY_API_KEY environment variable, or ~/secrets/deploy.env"
                ),
                None => "DEPLOY_API_KEY environment variable or ~/secrets/deploy.env".to_string(),
            };
            eprintln!("No API key found in {sources}");
        }
    }

    Ok(ClientSetup {
        client,
        project_name,
        dest_url,
        local_dir,
        config_text,
        settings,
        files,
    })
}

/// Resolves the include/exclude rules against the local root and validates the
/// result against the security scan.
pub fn resolve_local_files(
    local_dir: &Path,
    config_text: &str,
    settings: &ClientSettings,
) -> Result<FileList> {
    let files = deploy_core::filelist::resolve_file_list_from_config(local_dir, config_text)?;
    security::validate_file_list(&files.rel_paths(), &settings.ignore_security_scan)?;
    Ok(files)
}

/// Hashes every file in the list, producing the manifest sent to the server.
/// Port of `getSourceManifest`.
pub fn source_manifest(files: &FileList) -> Result<Vec<FileEntry>> {
    let mut manifest = Vec::with_capacity(files.len());

    for file in files.list_all() {
        let sha = get_file_hash(&file.source_path)
            .with_context(|| format!("Could not hash {}", file.source_path.display()))?
            .ok_or_else(|| {
                // The walk found it moments ago, so a missing file here means
                // something is rewriting the tree mid-deploy.
                anyhow!("File disappeared while building the manifest: {}", file.source_path.display())
            })?;

        manifest.push(FileEntry {
            rel_path: file.rel_path.clone(),
            sha,
        });
    }

    Ok(manifest)
}

/// Port of src/utils/RunningTimer.ts: elapsed seconds since the last check.
pub struct RunningTimer {
    last_check: Instant,
}

impl RunningTimer {
    pub fn new() -> RunningTimer {
        RunningTimer {
            last_check: Instant::now(),
        }
    }

    pub fn check_elapsed_secs(&mut self) -> f64 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_check);
        self.last_check = now;
        elapsed.as_millis() as f64 / 1000.0
    }
}

impl Default for RunningTimer {
    fn default() -> Self {
        RunningTimer::new()
    }
}
