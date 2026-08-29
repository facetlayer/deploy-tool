//! `deploy start-services` / `deploy preview-start-services`.
//! Port of src/commands/startServices.ts.
//!
//! This runs on a deploy host, not on a developer laptop: it reads the server's
//! own state database to find every active deployment that declares a
//! `candle-config`, and runs `candle check-start` in each one. It is what
//! brings services back after a reboot.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use deploy_core::config::parse_activation_config;
use rusqlite::{Connection, OpenFlags};

use crate::shell::run_shell_command;

const CANDLE_COMMAND: &str = "candle check-start";

#[derive(Clone, Debug)]
pub struct CandleService {
    pub project_name: String,
    pub deploy_name: String,
    pub deploy_dir: PathBuf,
    pub candle_config_path: String,
    pub command: String,
}

/// Resolution order for the server's state directory. Mirrors
/// `deploy-server`'s `db::state_directory`; the CLI cannot call it because
/// deploy-server is a binary-only crate.
fn state_directory() -> PathBuf {
    if let Ok(dir) = std::env::var("DEPLOY_STATE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("deploy");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local").join("state").join("deploy")
}

/// Opens the server database read-only: this is a client process, and it must
/// never create or migrate the server's schema as a side effect of being run on
/// the wrong machine.
fn open_state_database() -> Result<Connection> {
    let path = state_directory().join("db.sqlite");
    if !path.exists() {
        bail!(
            "No deploy server database at {}. start-services has to run on a deploy host.",
            path.display()
        );
    }

    Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Could not open {}", path.display()))
}

pub fn collect_candle_services() -> Result<Vec<CandleService>> {
    let conn = open_state_database()?;

    let deployments_dir: String = conn
        .query_row("select deployments_dir from deployments_dir", [], |row| {
            row.get(0)
        })
        .context("Deployments directory has not been configured")?;
    let deployments_dir = PathBuf::from(deployments_dir);

    let mut stmt = conn.prepare(
        "select ad.project_name, ad.deploy_name, d.deploy_dir, d.source_config_file
         from active_deployment ad
         join deployment d on d.deploy_name = ad.deploy_name
         order by ad.project_name",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut services = Vec::new();

    for row in rows {
        let (project_name, deploy_name, deploy_dir, source_config_file) = row?;

        let Some(config_text) = source_config_file else {
            continue;
        };

        let parsed = parse_activation_config(&config_text);
        let Some(candle_config_path) = parsed.candle_config_path else {
            continue;
        };

        services.push(CandleService {
            project_name,
            deploy_name,
            deploy_dir: deployments_dir.join(deploy_dir),
            candle_config_path,
            command: CANDLE_COMMAND.to_string(),
        });
    }

    Ok(services)
}

pub fn start_services() -> Result<()> {
    let services = collect_candle_services()?;

    if services.is_empty() {
        println!("No active deployments with a candle-config found.");
        return Ok(());
    }

    let mut failures = 0;

    for service in &services {
        println!();
        println!(
            "[{}] (deploy: {})",
            service.project_name, service.deploy_name
        );
        println!("  cwd: {}", service.deploy_dir.display());
        println!("  $ {}", service.command);

        if !Path::new(&service.deploy_dir).is_dir() {
            eprintln!(
                "deploy directory does not exist: {}",
                service.deploy_dir.display()
            );
            failures += 1;
            continue;
        }

        // One service failing must not stop the rest from coming up, so this
        // records the failure and keeps going.
        match run_shell_command(&service.command, &service.deploy_dir) {
            Ok(status) if status.success() => println!("  ok"),
            Ok(status) => {
                eprintln!(
                    "{}: candle check-start exited {}",
                    service.project_name,
                    status.code().map(|c| c.to_string()).unwrap_or_else(|| "on a signal".to_string())
                );
                failures += 1;
            }
            Err(err) => {
                eprintln!("{}: {err:#}", service.project_name);
                failures += 1;
            }
        }
    }

    if failures > 0 {
        bail!("start-services completed with {failures} failure(s).");
    }

    Ok(())
}

pub fn preview_start_services() -> Result<()> {
    let services = collect_candle_services()?;

    if services.is_empty() {
        println!("No active deployments with a candle-config found.");
        return Ok(());
    }

    println!("Would start {} service(s):", services.len());
    println!();

    for service in &services {
        println!("[{}] (deploy: {})", service.project_name, service.deploy_name);
        println!("  cwd: {}", service.deploy_dir.display());
        println!("  candle-config: {}", service.candle_config_path);
        println!("  $ {}", service.command);
        println!();
    }

    Ok(())
}
