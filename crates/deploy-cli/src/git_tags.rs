//! Git metadata recorded on a deployment. Port of src/client/gitTags.ts.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};
use deploy_core::config::ClientSettings;
use deploy_core::rpc::{tag_names, DeploymentTags};

struct GitOutput {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn run_git(args: &[&str], local_dir: &Path) -> Result<GitOutput> {
    let output = Command::new("git").args(args).current_dir(local_dir).output()?;

    Ok(GitOutput {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// The tags this deploy should carry, per the config's settings.
///
/// When `track-git-commit` is enabled this checks that the working tree is
/// clean (unless `allow-dirty-git-tree` is also set) and records the current
/// commit. Callers run this before any build work, so a dirty tree fails fast
/// rather than after a long `before-deploy` hook.
pub fn collect_deployment_tags(
    settings: &ClientSettings,
    local_dir: &Path,
) -> Result<DeploymentTags> {
    let mut tags = DeploymentTags::new();

    if !settings.track_git_commit {
        return Ok(tags);
    }

    let rev_parse = run_git(&["rev-parse", "HEAD"], local_dir)?;
    if !rev_parse.ok {
        bail!(
            "track-git-commit is enabled but 'git rev-parse HEAD' failed in {}: {}",
            local_dir.display(),
            rev_parse.stderr
        );
    }
    let commit = rev_parse.stdout;

    if !settings.allow_dirty_git_tree {
        let status = run_git(&["status", "--porcelain"], local_dir)?;
        if !status.ok {
            bail!(
                "track-git-commit is enabled but 'git status' failed in {}: {}",
                local_dir.display(),
                status.stderr
            );
        }

        let dirty_lines: Vec<&str> = status
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();

        if !dirty_lines.is_empty() {
            let shown = dirty_lines
                .iter()
                .take(20)
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            let more = if dirty_lines.len() > 20 {
                format!("\n  ...and {} more", dirty_lines.len() - 20)
            } else {
                String::new()
            };

            bail!(
                "Refusing to deploy: the git working tree at {} has uncommitted changes.\n\
                 {shown}{more}\n\n\
                 Commit or stash these changes, or add 'allow-dirty-git-tree' to deploy-settings.",
                local_dir.display()
            );
        }
    }

    tags.insert(tag_names::GIT_COMMIT.to_string(), commit);

    // A detached HEAD reports the branch as "HEAD", which is not worth
    // recording.
    let branch_result = run_git(&["rev-parse", "--abbrev-ref", "HEAD"], local_dir)?;
    if branch_result.ok && !branch_result.stdout.is_empty() && branch_result.stdout != "HEAD" {
        tags.insert(tag_names::GIT_BRANCH.to_string(), branch_result.stdout);
    }

    Ok(tags)
}

/// The local `HEAD` commit, or `None` when this is not a usable git checkout.
/// Backs `check-deployed-commit`.
pub fn local_head_commit(local_dir: &Path) -> Option<String> {
    let result = run_git(&["rev-parse", "HEAD"], local_dir).ok()?;
    if !result.ok || result.stdout.is_empty() {
        return None;
    }
    Some(result.stdout)
}
