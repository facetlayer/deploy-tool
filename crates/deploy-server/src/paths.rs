//! Path helpers for the deployments directory.
//!
//! Ported from the old daemon's `paths.rs` (itself a port of
//! `src/server/deployDirs.ts`). The containment checks here are the only thing
//! stopping a client-supplied `relPath` from writing outside its deployment, so
//! they are kept exactly as they were.

use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::db;

/// Lexical normalization equivalent to Node's `path.join` + `path.resolve`:
/// `.` and `..` are resolved textually, without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }

    out
}

/// Joins a relative path into a directory and refuses anything that escapes it.
///
/// Path traversal guard. `dir.join(rel_path)` also handles the absolute-path
/// case: an absolute `rel_path` replaces `dir` entirely and then fails the
/// containment check below.
pub fn get_safe_path_in_dir(dir: &Path, rel_path: &str) -> Result<PathBuf> {
    let joined = if rel_path.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel_path)
    };

    let resolved_path = normalize(&joined);
    let resolved_dir = normalize(dir);

    if resolved_path != resolved_dir && !resolved_path.starts_with(&resolved_dir) {
        return Err(anyhow!("Invalid path: {}", rel_path));
    }

    Ok(resolved_path)
}

/// Resolves a relative path inside a named deployment's directory.
pub fn get_path_in_deployment_dir(
    conn: &Connection,
    deploy_name: &str,
    rel_path: &str,
) -> Result<PathBuf> {
    let deploy_dir: Option<String> = conn
        .query_row(
            "select deploy_dir from deployment where deploy_name = ?",
            [deploy_name],
            |row| row.get(0),
        )
        .optional()?;

    let deploy_dir =
        deploy_dir.ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?;

    let full_deploy_dir = db::get_deployments_dir(conn)?.join(deploy_dir);
    get_safe_path_in_dir(&full_deploy_dir, rel_path)
}

/// The directory of a project's active deployment, or None when it has none.
pub fn get_active_deployment_dir(conn: &Connection, project_name: &str) -> Result<Option<PathBuf>> {
    let active: Option<String> = conn
        .query_row(
            "select deploy_name from active_deployment where project_name = ?",
            [project_name],
            |row| row.get(0),
        )
        .optional()?;

    let active = match active {
        Some(name) => name,
        None => return Ok(None),
    };

    let deploy_dir: Option<String> = conn
        .query_row(
            "select deploy_dir from deployment where deploy_name = ?",
            [active],
            |row| row.get(0),
        )
        .optional()?;

    match deploy_dir {
        Some(dir) => Ok(Some(db::get_deployments_dir(conn)?.join(dir))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPLOY_DIR: &str = "/srv/deployments/my-project";

    #[test]
    fn rejects_dot_dot_traversal() {
        let err = get_safe_path_in_dir(Path::new(DEPLOY_DIR), "../../../etc/passwd").unwrap_err();
        assert!(err.to_string().contains("Invalid path"));
    }

    #[test]
    fn rejects_traversal_into_a_sibling_deployment() {
        assert!(get_safe_path_in_dir(Path::new(DEPLOY_DIR), "../other-project/config.json").is_err());
    }

    #[test]
    fn rejects_absolute_path_injection() {
        assert!(get_safe_path_in_dir(Path::new(DEPLOY_DIR), "/etc/passwd").is_err());
    }

    #[test]
    fn allows_normal_relative_paths() {
        assert_eq!(
            get_safe_path_in_dir(Path::new(DEPLOY_DIR), "src/index.js").unwrap(),
            PathBuf::from("/srv/deployments/my-project/src/index.js")
        );
    }

    #[test]
    fn allows_nested_directory_paths() {
        assert_eq!(
            get_safe_path_in_dir(Path::new(DEPLOY_DIR), "src/components/Header.tsx").unwrap(),
            PathBuf::from("/srv/deployments/my-project/src/components/Header.tsx")
        );
    }

    #[test]
    fn allows_traversal_that_stays_inside_the_directory() {
        assert_eq!(
            get_safe_path_in_dir(Path::new(DEPLOY_DIR), "src/../package.json").unwrap(),
            PathBuf::from("/srv/deployments/my-project/package.json")
        );
    }

    #[test]
    fn empty_rel_path_is_the_directory_itself() {
        assert_eq!(
            get_safe_path_in_dir(Path::new(DEPLOY_DIR), "").unwrap(),
            PathBuf::from(DEPLOY_DIR)
        );
    }

    #[test]
    fn normalize_folds_dot_and_dot_dot() {
        assert_eq!(
            normalize(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
    }
}
