//! `deploy-server` — the deploy backend. One instance per host.
//!
//! Subcommands ported from `~/tools/deploy/rust/src/main.rs`, plus
//! `list-legacy-keys`, which R6 requires so an operator can see which local
//! `secret_key` rows still have to migrate before the table is turned off.

mod auth_center;
mod authz;
mod db;
mod handlers;
mod manifest;
mod paths;
mod preserve;
mod server;
mod sql;
mod state;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use crate::auth_center::{AuthCenter, AuthCenterConfig};

#[derive(Parser)]
#[command(name = "deploy-server", version, about = "Deploy backend API server")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the deploy server
    Serve {
        /// Disable API key validation. Development only.
        #[arg(long)]
        disable_api_key_check: bool,
        /// Port number for the server
        #[arg(long)]
        port: u16,
    },
    /// Set the directory where deployments are stored
    SetDeploymentsDir { directory: String },
    /// Generate a new legacy secret API key in the local table
    CreateKey,
    /// List the local secret keys that still exist, so the migration to
    /// auth-center can be finished (R6). Never prints the key text.
    ListLegacyKeys,
    /// Dump the parsed form of a .deploy config as JSON. Used to diff this
    /// parser against the TypeScript @facetlayer/qc implementation.
    #[command(hide = true)]
    DebugParseConfig { file: String },
}

fn debug_parse_config(file: &str) -> Result<()> {
    let text = std::fs::read_to_string(file)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&deploy_core::qc::debug_dump(&text))?
    );
    Ok(())
}

fn set_deployments_dir(directory: &str) -> Result<()> {
    let absolute_path = std::path::Path::new(directory)
        .canonicalize()
        .map_err(|_| anyhow!("Directory does not exist: {}", directory))?;

    if !absolute_path.is_dir() {
        return Err(anyhow!(
            "Path is not a directory: {}",
            absolute_path.display()
        ));
    }

    let conn = db::open_database()?;
    conn.execute("delete from deployments_dir", [])?;
    conn.execute(
        "insert into deployments_dir (deployments_dir, created_at) values (?, ?)",
        rusqlite::params![absolute_path.to_string_lossy().to_string(), db::now_iso()],
    )?;

    println!("Deployments directory set to: {}", absolute_path.display());
    Ok(())
}

fn create_key() -> Result<()> {
    use rand::RngCore;

    let mut bytes = [0u8; 30];
    rand::thread_rng().fill_bytes(&mut bytes);
    let key_text = hex::encode(bytes);

    let conn = db::open_database()?;
    conn.execute(
        "insert into secret_key (key_text, created_at) values (?, ?)",
        rusqlite::params![&key_text, db::now_iso()],
    )?;

    println!("Generated new secret key: {}", key_text);
    Ok(())
}

/// R6. Deliberately does not select `key_text`: this command exists to plan a
/// migration, not to recover key material.
fn list_legacy_keys() -> Result<()> {
    let conn = db::open_database()?;
    let mut stmt = conn.prepare(
        "select key_id, label, created_at, last_used_at from secret_key order by key_id",
    )?;
    let rows: Vec<(i64, Option<String>, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<std::result::Result<_, _>>()?;

    if rows.is_empty() {
        println!(
            "No legacy secret keys remain. This instance can set DEPLOY_DISABLE_LEGACY_KEYS=1."
        );
        return Ok(());
    }

    println!(
        "{:<8} {:<24} {:<26} {}",
        "ID", "LABEL", "CREATED", "LAST USED"
    );
    for (key_id, label, created_at, last_used_at) in &rows {
        println!(
            "{:<8} {:<24} {:<26} {}",
            key_id,
            label.as_deref().unwrap_or("-"),
            created_at,
            last_used_at.as_deref().unwrap_or("never")
        );
    }
    println!();
    println!(
        "{} legacy key(s) remain. Each grants every action on this instance.",
        rows.len()
    );
    if !authz::legacy_keys_enabled() {
        println!("DEPLOY_DISABLE_LEGACY_KEYS=1 is set, so none of them are currently accepted.");
    }
    Ok(())
}

/// Printed at startup because a misconfigured auth setup is a security problem
/// and should be visible in the journal rather than inferred from behavior.
fn print_auth_summary(config: Option<&AuthCenterConfig>, disable_api_key_check: bool) {
    println!("--- authorization ---");
    match config {
        Some(config) => {
            println!("  auth-center:      {}", config.base_url);
            println!("  admin resource:   {}", config.admin_resource);
        }
        None => {
            println!("  auth-center:      not configured (legacy-only)");
            println!("  admin resource:   n/a; createProject is denied");
        }
    }
    if authz::legacy_keys_enabled() {
        println!("  legacy keys:      ENABLED (local secret_key table grants everything)");
    } else {
        println!("  legacy keys:      disabled (DEPLOY_DISABLE_LEGACY_KEYS=1)");
    }
    if config.is_none() && !authz::legacy_keys_enabled() && !disable_api_key_check {
        println!("  WARNING: no auth-center and no legacy keys: every call will be denied.");
    }
    if disable_api_key_check {
        println!("  WARNING: --disable-api-key-check is set; every call is allowed.");
    }
    println!("---------------------");
}

fn serve(disable_api_key_check: bool, port: u16) -> Result<()> {
    // Refuse to start half-configured rather than silently falling back to
    // legacy-only, which would look like it was working.
    let config = AuthCenterConfig::from_env()?;
    print_auth_summary(config.as_ref(), disable_api_key_check);
    auth_center::install(config.map(AuthCenter::new));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(server::start_server(server::StartServerOptions {
        disable_api_key_check,
        port,
    }))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            disable_api_key_check,
            port,
        } => serve(disable_api_key_check, port),
        Commands::SetDeploymentsDir { directory } => set_deployments_dir(&directory),
        Commands::CreateKey => create_key(),
        Commands::ListLegacyKeys => list_legacy_keys(),
        Commands::DebugParseConfig { file } => debug_parse_config(&file),
    }
}
