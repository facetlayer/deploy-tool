//! Activation: point a project at a deployment and run its after-deploy hooks.
//! Port of `src/server/activateDeployment.ts`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};
use rusqlite::OptionalExtension;
use serde_json::Value as Json;

use deploy_core::config::{parse_activation_config, AfterDeployAction};
use deploy_core::rpc::ActivateDeploymentParams;

use crate::db;
use crate::handlers::parse_params;
use crate::paths::get_safe_path_in_dir;
use crate::state::AppState;

/// Locate the candle binary without relying on PATH.
///
/// Non-interactive SSH sessions often lack the user's full shell profile, so
/// `candle` may not be on PATH even when it is installed. Check the common
/// install locations first and fall back to `which candle`.
fn find_candle_binary() -> String {
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/usr/local/bin/candle"),
        PathBuf::from("/usr/bin/candle"),
        PathBuf::from("/root/.local/bin/candle"),
        PathBuf::from("/root/.npm-global/bin/candle"),
    ];

    if let Ok(entries) = std::fs::read_dir("/home") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            candidates.push(PathBuf::from("/home").join(&name).join(".local/bin/candle"));
            candidates.push(
                PathBuf::from("/home")
                    .join(&name)
                    .join(".npm-global/bin/candle"),
            );
        }
    }

    for candidate in &candidates {
        if is_executable(candidate) {
            return candidate.to_string_lossy().to_string();
        }
    }

    if let Ok(output) = Command::new("sh").arg("-c").arg("which candle").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return path;
            }
        }
    }

    "candle".to_string()
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn execute_shell_command(command: &str, working_dir: &Path, hook_type: &str) -> Result<()> {
    println!(
        "Running {} command (cwd: {}): {}",
        hook_type,
        working_dir.display(),
        command
    );

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            println!("[{}]  {}", hook_type, line);
        }
    }
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("[{}]  {}", hook_type, line);
        }
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!(
            "{} command failed with exit code: {}",
            hook_type,
            status.code().unwrap_or(-1)
        ));
    }

    Ok(())
}

fn install_candle_config(deploy_dir: &Path, candle_config_path: &str) -> Result<()> {
    let source_file = get_safe_path_in_dir(deploy_dir, candle_config_path)?;
    let dest_file = deploy_dir.join(".candle.json");

    if !source_file.exists() {
        return Err(anyhow!(
            "candle-config file not found in deployment: {}. Make sure this file is included in the deploy.",
            candle_config_path
        ));
    }

    println!(
        "Installing candle config: {} -> .candle.json",
        candle_config_path
    );
    std::fs::copy(&source_file, &dest_file)?;
    Ok(())
}

/// Activates a deployment and then runs its after-deploy hooks.
///
/// Behavioral choice inherited from the old Rust daemon: this waits for the
/// hooks to finish before responding. The TypeScript server streamed hook
/// output back instead, so its client saw the response first. Waiting means a
/// `deploy` command that returns has actually finished restarting the service.
pub fn activate_deployment(state: &AppState, params: &Json) -> Result<Json> {
    let params: ActivateDeploymentParams = parse_params(params)?;
    let deploy_name = params.deploy_name;

    let (deploy_dir, source_config, deployment_project_name) = {
        let conn = state.db();
        let row: Option<(String, Option<String>, String)> = conn
            .query_row(
                "select deploy_dir, source_config_file, project_name
                 from deployment where deploy_name = ?",
                [&deploy_name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        let (deploy_dir, source_config, project_name) =
            row.ok_or_else(|| anyhow!("Deployment not found: {}", deploy_name))?;

        (
            db::get_deployments_dir(&conn)?.join(deploy_dir),
            source_config.unwrap_or_default(),
            project_name,
        )
    };

    let activation = parse_activation_config(&source_config);

    {
        let conn = state.db();
        let project_exists: Option<String> = conn
            .query_row(
                "select project_name from project where project_name = ?",
                [&activation.project_name],
                |row| row.get(0),
            )
            .optional()?;

        if project_exists.is_none() {
            return Err(anyhow!(
                "No record found for project {}",
                activation.project_name
            ));
        }

        // Upsert active_deployment BEFORE running after-deploy hooks, so the
        // pointer always advances to the newest successful upload regardless of
        // hook outcomes.
        conn.execute(
            "insert into active_deployment (project_name, deploy_name, updated_at)
             values (?, ?, ?)
             on conflict(project_name) do update set
                deploy_name = excluded.deploy_name,
                updated_at = excluded.updated_at",
            rusqlite::params![deployment_project_name, deploy_name, db::now_iso()],
        )?;
    }

    println!("Successfully activated deployment: {}", deploy_name);

    let mut hook_errors: Vec<String> = Vec::new();

    let needs_candle = activation
        .after_deploy_actions
        .iter()
        .any(|action| matches!(action, AfterDeployAction::CandleRestart { .. }))
        || activation.candle_config_path.is_some();
    let candle_bin = if needs_candle {
        find_candle_binary()
    } else {
        "candle".to_string()
    };

    for action in &activation.after_deploy_actions {
        let result = match action {
            AfterDeployAction::Shell { command } => {
                execute_shell_command(command, &deploy_dir, "after-deploy")
            }
            AfterDeployAction::CandleRestart { service_name } => execute_shell_command(
                &format!("{} restart {}", candle_bin, service_name),
                &deploy_dir,
                "after-deploy:candle-restart",
            ),
        };

        if let Err(error) = result {
            let message = error.to_string();
            eprintln!("[deploy error] after-deploy hook error: {}", message);
            hook_errors.push(message);
        }
    }

    // Everything past this point is best-effort: the deployment is already
    // active, and the old server also reported success to the client regardless
    // of what the hooks did.
    if let Some(candle_config_path) = &activation.candle_config_path {
        if let Err(error) = install_candle_config(&deploy_dir, candle_config_path) {
            let message = error.to_string();
            eprintln!("[deploy error] candle-config error: {}", message);
            hook_errors.push(message);
        }

        // Restart any running services defined in the candle config, then start
        // any that aren't running yet.
        if let Err(error) = execute_shell_command(
            &format!("{} restart", candle_bin),
            &deploy_dir,
            "candle-config:restart",
        ) {
            let message = error.to_string();
            eprintln!(
                "[deploy error] candle-config:restart hook error: {}",
                message
            );
            hook_errors.push(message);
        }

        if let Err(error) = execute_shell_command(
            &format!("{} check-start", candle_bin),
            &deploy_dir,
            "candle-config:check-start",
        ) {
            let message = error.to_string();
            eprintln!(
                "[deploy error] candle-config:check-start hook error: {}",
                message
            );
            hook_errors.push(message);
        }
    }

    if !hook_errors.is_empty() {
        println!(
            "[deploy warn] Deployment is active, but some after-deploy hooks failed:\n{}",
            hook_errors
                .iter()
                .map(|error| format!("  - {}", error))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    // The old TS handler returned a Stream, which serialized to an object the
    // client ignored. Null keeps the JSON-RPC contract (a defined result) intact.
    Ok(Json::Null)
}
