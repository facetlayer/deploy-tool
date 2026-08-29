//! `deploy rollback`. Port of src/client/rollback.ts.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{bail, Result};
use deploy_core::rpc::*;

use crate::client_setup::setup_client;

pub fn rollback(
    config_file: &Path,
    deploy_name: Option<&str>,
    override_dest: Option<&str>,
    limit: i64,
) -> Result<()> {
    let setup = setup_client(config_file, override_dest)?;

    let result = setup.client.list_deployments(&ListDeploymentsParams {
        project_name: setup.project_name.clone(),
        limit: Some(limit),
    })?;

    if result.deployments.is_empty() {
        println!("No deployments found for project '{}'.", setup.project_name);
        return Ok(());
    }

    let target = match deploy_name {
        Some(name) => {
            // Rolling back to a name the server did not list is almost always a
            // typo, and is worth catching before the RPC.
            if !result.deployments.iter().any(|d| d.deploy_name == name) {
                bail!(
                    "Deployment '{name}' not found in the recent history for project '{}'.\n\
                     Use 'deploy rollback <config-file>' (without a deploy name) to see \
                     available deployments.",
                    setup.project_name
                );
            }
            name.to_string()
        }
        None => match choose_deployment(&setup.project_name, &result.deployments)? {
            Some(name) => name,
            None => {
                println!("Rollback cancelled.");
                return Ok(());
            }
        },
    };

    if result
        .deployments
        .iter()
        .any(|d| d.deploy_name == target && d.is_active)
    {
        println!("Deployment '{target}' is already the active deployment. Nothing to do.");
        return Ok(());
    }

    println!();
    println!(
        "Rolling back project '{}' to deployment: {target}",
        setup.project_name
    );

    setup.client.rollback(&RollbackParams {
        project_name: setup.project_name.clone(),
        deploy_name: target.clone(),
    })?;

    println!("Rollback complete. Active deployment is now: {target}");
    Ok(())
}

/// Prints the recent deployments and reads a selection from stdin. `None` means
/// the user cancelled with an empty line.
fn choose_deployment(project_name: &str, deployments: &[DeploymentInfo]) -> Result<Option<String>> {
    println!();
    println!("Recent deployments for project '{project_name}':");
    println!();

    for (index, deployment) in deployments.iter().enumerate() {
        let active_marker = if deployment.is_active {
            " (active)"
        } else {
            ""
        };
        println!(
            "  {}. {}  [{}]{active_marker}",
            index + 1,
            deployment.deploy_name,
            deployment.created_at
        );
    }

    println!();
    print!("Enter deployment number to roll back to (or press Enter to cancel): ");
    std::io::stdout().flush()?;

    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    let answer = answer.trim();

    if answer.is_empty() {
        return Ok(None);
    }

    match parse_selection(answer, deployments.len()) {
        Some(index) => Ok(Some(deployments[index].deploy_name.clone())),
        None => bail!(
            "Invalid selection: '{answer}'. Please enter a number between 1 and {}.",
            deployments.len()
        ),
    }
}

/// Converts a 1-based menu answer into a 0-based index.
fn parse_selection(answer: &str, count: usize) -> Option<usize> {
    let number: usize = answer.parse().ok()?;
    if number == 0 || number > count {
        return None;
    }
    Some(number - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_one_based() {
        assert_eq!(parse_selection("1", 3), Some(0));
        assert_eq!(parse_selection("3", 3), Some(2));
    }

    #[test]
    fn out_of_range_and_non_numeric_answers_are_rejected() {
        assert_eq!(parse_selection("0", 3), None);
        assert_eq!(parse_selection("4", 3), None);
        assert_eq!(parse_selection("-1", 3), None);
        assert_eq!(parse_selection("abc", 3), None);
    }
}
