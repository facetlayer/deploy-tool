//! `deploy copy-back`. Port of src/client/copyBack.ts.

use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use deploy_core::rpc::*;

use crate::client_setup::setup_client;

pub fn copy_back(config_file: &Path, filename: &str, override_dest: Option<&str>) -> Result<()> {
    let setup = setup_client(config_file, override_dest)?;

    // The server names the file it sends back, but the local write path is
    // built here: a `..` or absolute path would let a hostile or buggy server
    // land a file anywhere on the deploying machine.
    reject_escaping_path(filename)?;

    println!(
        "Downloading {filename} from {} at {}...",
        setup.project_name, setup.dest_url
    );

    let result = setup.client.download_file(&DownloadFileParams {
        project_name: setup.project_name.clone(),
        rel_path: filename.to_string(),
    })?;

    let local_path = setup.local_root.join(filename);
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }

    let content = BASE64
        .decode(result.content_base64.as_bytes())
        .context("Server sent a file that is not valid base64")?;
    std::fs::write(&local_path, content)
        .with_context(|| format!("Could not write {}", local_path.display()))?;

    println!("Saved to {}", local_path.display());
    Ok(())
}

fn reject_escaping_path(rel_path: &str) -> Result<()> {
    let path = Path::new(rel_path);

    if path.is_absolute() {
        bail!("Refusing to copy back an absolute path: {rel_path}");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("Refusing to copy back a path containing '..': {rel_path}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_relative_paths_are_allowed() {
        assert!(reject_escaping_path("data/app.sqlite").is_ok());
        assert!(reject_escaping_path("app.sqlite").is_ok());
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused() {
        assert!(reject_escaping_path("../../etc/passwd").is_err());
        assert!(reject_escaping_path("data/../../escape").is_err());
        assert!(reject_escaping_path("/etc/passwd").is_err());
    }
}
