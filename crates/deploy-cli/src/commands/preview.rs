//! `deploy preview` and `deploy preview-deploy-files`.
//! Port of src/client/previewDeploy.ts and src/client/deployPreviewFiles.ts.

use std::path::Path;

use anyhow::Result;
use deploy_core::config::{self, AfterDeployAction};
use deploy_core::rpc::*;

use crate::client_setup::{setup_client, source_manifest};
use crate::commands::run::{send_manifest_batches, MANIFEST_BATCH_SIZE};

/// Shows the local file set without contacting the server at all.
pub fn preview_deploy_files(config_file: &Path) -> Result<()> {
    let config_text = std::fs::read_to_string(config_file)?;
    let settings = config::parse_client_settings(&config_text);
    let local_root = config::resolve_local_root(config_file, &settings, None)?;
    let files = crate::client_setup::resolve_local_files(&local_root, &config_text, &settings)?;

    println!(
        "Project: {}",
        settings.project_name.as_deref().unwrap_or("(unset)")
    );
    println!(
        "Destination: {}",
        settings.dest_url.as_deref().unwrap_or("(unset)")
    );
    println!("Local directory: {}", local_root.display());
    println!("Files to upload ({}):", files.len());
    println!();

    for file in files.list_all() {
        println!("  {}", file.rel_path);
    }

    Ok(())
}

/// Shows the drift between the local file set and what the server already has.
pub fn preview(config_file: &Path, override_dest: Option<&str>) -> Result<()> {
    let setup = setup_client(config_file, override_dest)?;

    println!("Project: {}", setup.project_name);
    println!("Destination: {}", setup.dest_url);
    println!();

    let manifest = source_manifest(&setup.files)?;

    let result = if manifest.len() > MANIFEST_BATCH_SIZE {
        // A manifest too large to send inline needs somewhere to live, so the
        // preview creates a deployment to hold it and previews by name. The
        // deployment is never activated.
        let created = setup.client.create_deployment(&CreateDeploymentParams {
            project_name: setup.project_name.clone(),
            source_file_manifest: Vec::new(),
            source_file_config: setup.config_text.clone(),
            tags: None,
        })?;

        send_manifest_batches(&setup.client, &created.deploy_name, &manifest)?;

        setup
            .client
            .preview_by_deploy_name(&PreviewByDeployNameParams {
                deploy_name: created.deploy_name,
            })?
    } else {
        setup.client.preview_deployment(&PreviewDeploymentParams {
            project_name: setup.project_name.clone(),
            source_file_manifest: manifest,
            source_file_config: setup.config_text.clone(),
        })?
    };

    if result.files_to_upload.is_empty() && result.files_to_delete.is_empty() {
        println!("No drift detected. Server is up to date.");
    } else {
        if !result.files_to_upload.is_empty() {
            println!("Files to upload ({}):", result.files_to_upload.len());
            for file in &result.files_to_upload {
                println!("  + {}", file.rel_path);
            }
            println!();
        }

        if !result.files_to_delete.is_empty() {
            println!(
                "Server files to be deleted ({}):",
                result.files_to_delete.len()
            );
            for file in &result.files_to_delete {
                println!("  - {file}");
            }
            println!();
        }

        println!(
            "Summary: {} to upload, {} to delete",
            result.files_to_upload.len(),
            result.files_to_delete.len()
        );
        println!();
    }

    print_before_hooks(&setup.settings.before_deploy_commands);
    print_after_hooks(&config::parse_activation_config(&setup.config_text).after_deploy_actions);

    Ok(())
}

fn print_before_hooks(commands: &[String]) {
    if commands.is_empty() {
        return;
    }
    println!("Before-deploy hooks ({}):", commands.len());
    for command in commands {
        println!("  $ {command}");
    }
    println!();
}

fn print_after_hooks(actions: &[AfterDeployAction]) {
    if actions.is_empty() {
        return;
    }
    println!("After-deploy hooks ({}):", actions.len());
    for action in actions {
        match action {
            AfterDeployAction::Shell { command } => println!("  $ {command}"),
            AfterDeployAction::CandleRestart { service_name } => {
                println!("  candle restart {service_name}")
            }
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of src/__tests__/multipleShellCommands.test.ts. The hazard it
    /// guards is real: reading `shell` with `get_attr` returns only the last
    /// one, silently dropping every earlier command in the block.
    fn after_deploy_actions(config: &str) -> Vec<AfterDeployAction> {
        config::parse_activation_config(config).after_deploy_actions
    }

    #[test]
    fn collects_every_shell_command_in_an_after_deploy_block() {
        let actions = after_deploy_actions(
            "after-deploy\n  shell(echo first)\n  shell(echo second)\n  shell(echo third)\n",
        );
        let commands: Vec<&str> = actions
            .iter()
            .map(|action| match action {
                AfterDeployAction::Shell { command } => command.as_str(),
                _ => panic!("expected shell"),
            })
            .collect();
        assert_eq!(commands, vec!["echo first", "echo second", "echo third"]);
    }

    #[test]
    fn collects_every_shell_command_in_a_before_deploy_block() {
        let settings = config::parse_client_settings(
            "before-deploy\n  shell(pnpm build)\n  shell(pnpm lint)\n",
        );
        assert_eq!(
            settings.before_deploy_commands,
            vec!["pnpm build".to_string(), "pnpm lint".to_string()]
        );
    }

    #[test]
    fn parses_candle_restart_directives() {
        let actions = after_deploy_actions("after-deploy\n  candle-restart(my-service)\n");
        assert_eq!(
            actions,
            vec![AfterDeployAction::CandleRestart {
                service_name: "my-service".to_string()
            }]
        );
    }

    #[test]
    fn keeps_mixed_shell_and_candle_restart_directives_in_order() {
        let actions = after_deploy_actions(
            "after-deploy\n  shell(npm install --production)\n  candle-restart(my-api)\n",
        );
        assert_eq!(
            actions,
            vec![
                AfterDeployAction::Shell {
                    command: "npm install --production".to_string()
                },
                AfterDeployAction::CandleRestart {
                    service_name: "my-api".to_string()
                },
            ]
        );
    }

    #[test]
    fn works_with_a_single_shell_command() {
        let actions = after_deploy_actions("after-deploy\n  shell(echo only-one)\n");
        assert_eq!(
            actions,
            vec![AfterDeployAction::Shell {
                command: "echo only-one".to_string()
            }]
        );
    }
}
