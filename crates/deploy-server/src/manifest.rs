//! The deployment manifest as the server stores it.
//!
//! The old daemon carried its own `FileEntry`, glob rules, hashing and
//! directory-creation code here, duplicating the TypeScript client. All of that
//! now lives in `deploy_core` (`rpc::FileEntry`, `filelist`, `hash`); what is
//! left is the narrow job of moving a manifest between the
//! `deployment.manifest_json` column and the file list the rest of the server
//! works with.

use std::path::Path;

use anyhow::Result;
use deploy_core::filelist::{self, FileList};
use deploy_core::rpc::FileEntry;

/// Reads a manifest out of the `manifest_json` column.
///
/// A missing or corrupt blob yields an empty manifest rather than an error:
/// `database_cleanup` nulls out `manifest_json` for old deployments, so a
/// deployment with no manifest is an ordinary state, not a fault.
pub fn parse_manifest_json(manifest_json: Option<&str>) -> Vec<FileEntry> {
    match manifest_json {
        Some(text) => serde_json::from_str(text).unwrap_or_default(),
        None => Vec::new(),
    }
}

pub fn manifest_to_json(manifest: &[FileEntry]) -> Result<String> {
    Ok(serde_json::to_string(manifest)?)
}

/// The manifest as a `FileList`, which is what `deploy_core::filelist`'s
/// destination scan wants for "the files this deploy is shipping".
pub fn manifest_file_list(manifest: &[FileEntry]) -> FileList {
    FileList::from_rel_paths(manifest.iter().map(|file| file.rel_path.as_str()))
}

/// Creates every parent directory the manifest implies, so uploads can write
/// files without each creating its own. The traversal guard lives in
/// `deploy_core::filelist::setup_empty_directories`.
pub fn setup_empty_directories(target_dir: &Path, manifest: &[FileEntry]) -> Result<()> {
    filelist::setup_empty_directories(
        target_dir,
        manifest.iter().map(|file| file.rel_path.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_stored_manifest() {
        let manifest = parse_manifest_json(Some(r#"[{"relPath":"a.txt","sha":"aaa"}]"#));
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].rel_path, "a.txt");
    }

    #[test]
    fn a_missing_or_corrupt_manifest_is_empty_not_an_error() {
        assert!(parse_manifest_json(None).is_empty());
        assert!(parse_manifest_json(Some("not json")).is_empty());
    }

    #[test]
    fn ignores_extra_fields_from_an_older_client() {
        // The TypeScript CLI sends a `sourcePath` the server has no use for.
        let manifest = parse_manifest_json(Some(
            r#"[{"relPath":"a.txt","sha":"aaa","sourcePath":"/x/a.txt"}]"#,
        ));
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].rel_path, "a.txt");
    }

    #[test]
    fn setup_empty_directories_refuses_to_escape_the_deployment() {
        let dir = std::env::temp_dir().join("deploy-server-manifest-escape");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let manifest = vec![FileEntry {
            rel_path: "../escaped/file.txt".to_string(),
            sha: "aaa".to_string(),
        }];
        setup_empty_directories(&dir, &manifest).unwrap();

        assert!(!dir.parent().unwrap().join("escaped").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
