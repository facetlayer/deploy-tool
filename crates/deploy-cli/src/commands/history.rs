//! `deploy history` and `deploy check-deployed-commit`.
//! Port of src/client/history.ts and src/client/checkDeployedCommit.ts.

use std::path::Path;

use anyhow::{bail, Result};
use deploy_core::rpc::*;
use serde_json::json;

use crate::client_setup::setup_client;
use crate::git_tags::local_head_commit;

pub fn history(config_file: &Path, override_dest: Option<&str>, limit: i64) -> Result<()> {
    let setup = setup_client(config_file, override_dest)?;

    let result = setup.client.list_deployments(&ListDeploymentsParams {
        project_name: setup.project_name.clone(),
        limit: Some(limit),
    })?;

    if result.deployments.is_empty() {
        println!("No deployments found for project '{}'.", setup.project_name);
        return Ok(());
    }

    println!();
    println!("Deployment history for project '{}':", setup.project_name);
    println!();

    match &result.active_deploy_name {
        Some(name) => println!("  Active deployment: {name}"),
        None => println!("  No active deployment."),
    }
    println!();

    for deployment in &result.deployments {
        let active_marker = if deployment.is_active {
            " <- active"
        } else {
            ""
        };
        let commit_label = deployment
            .tags
            .as_ref()
            .and_then(|tags| tags.get(tag_names::GIT_COMMIT))
            .map(|commit| format!("  {}", short_commit(commit)))
            .unwrap_or_default();
        // R7: who authorized this deploy, when the server recorded it.
        let author = deployment
            .authorized_by_key_name
            .clone()
            .or_else(|| deployment.authorized_by_key_id.clone())
            .map(|who| format!("  by {who}"))
            .unwrap_or_default();

        println!(
            "  {}  [{}]{commit_label}{author}{active_marker}",
            deployment.deploy_name, deployment.created_at
        );
    }

    println!();
    Ok(())
}

pub fn check_deployed_commit(
    config_file: &Path,
    deploy_name: Option<&str>,
    json_output: bool,
    override_dest: Option<&str>,
) -> Result<()> {
    let setup = setup_client(config_file, override_dest)?;

    let result = setup.client.get_deployment_tags(&GetDeploymentTagsParams {
        project_name: setup.project_name.clone(),
        deploy_name: deploy_name.map(str::to_string),
    })?;

    let deployed_commit = result.tags.get(tag_names::GIT_COMMIT).cloned();
    let local_commit = local_head_commit(&setup.local_dir);

    if json_output {
        let matches_local = match (&deployed_commit, &local_commit) {
            (Some(deployed), Some(local)) => json!(deployed == local),
            _ => json!(null),
        };

        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "projectName": setup.project_name,
                "deployName": result.deploy_name,
                "createdAt": result.created_at,
                "isActive": result.is_active,
                "deployedCommit": deployed_commit,
                "localCommit": local_commit,
                "matchesLocal": matches_local,
                "tags": result.tags,
            }))?
        );
    } else {
        println!();
        println!("Project:     {}", setup.project_name);
        println!(
            "Deployment:  {}{}",
            result.deploy_name,
            if result.is_active { " (active)" } else { "" }
        );
        println!("Deployed at: {}", result.created_at);

        match &deployed_commit {
            Some(commit) => {
                let branch = result
                    .tags
                    .get(tag_names::GIT_BRANCH)
                    .map(|branch| format!(" (branch {branch})"))
                    .unwrap_or_default();
                println!("Commit:      {commit}{branch}");

                if let Some(local) = &local_commit {
                    println!("Local HEAD:  {local}");
                    println!();
                    if local == commit {
                        println!("Local HEAD matches the deployed commit.");
                    } else {
                        println!("Local HEAD does NOT match the deployed commit.");
                    }
                }
            }
            None => println!("Commit:      (not recorded)"),
        }

        println!();
    }

    if deployed_commit.is_none() {
        bail!(
            "No git commit is recorded for deployment '{}'. \
             Add 'track-git-commit' to the deploy-settings block and redeploy.",
            result.deploy_name
        );
    }

    Ok(())
}

fn short_commit(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_commit_truncates_to_eight_characters() {
        assert_eq!(short_commit("0123456789abcdef"), "01234567");
    }

    #[test]
    fn short_commit_tolerates_a_short_value() {
        assert_eq!(short_commit("abc"), "abc");
        assert_eq!(short_commit(""), "");
    }
}
