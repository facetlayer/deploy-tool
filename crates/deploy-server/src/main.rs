//! `deploy-server` — the deploy backend. One instance per host.
//!
//! Subcommands ported from `~/tools/deploy/rust/src/main.rs`. There is no
//! key-management subcommand: keys live in auth-center (R6).

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
        /// LOCAL DEVELOPMENT ONLY: accept every call without checking any API
        /// key. Never set this on do2 or dohl — it disables authorization
        /// outright, and it is not a way to keep old keys working.
        #[arg(long)]
        disable_api_key_check: bool,
        /// Port number for the server
        #[arg(long)]
        port: u16,
    },
    /// Set the directory where deployments are stored
    SetDeploymentsDir { directory: String },
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

/// Printed at startup because a misconfigured auth setup is a security problem
/// and should be visible in the journal rather than inferred from behavior.
fn print_auth_summary(config: &AuthCenterConfig, disable_api_key_check: bool) {
    println!("--- authorization ---");
    if disable_api_key_check {
        // Say nothing about the auth-center configuration here: with the check
        // disabled it is never consulted, and printing a URL next to the word
        // "authorization" reads as though calls were still being checked.
        println!("  ***********************************************************");
        println!("  * WARNING: --disable-api-key-check is set.                 *");
        println!("  * Every call is allowed, from anyone, with no key at all.  *");
        println!("  * auth-center is NOT consulted. Nothing is authorized.     *");
        println!("  * This is for local development only and must never be set *");
        println!("  * on do2 or dohl.                                          *");
        println!("  ***********************************************************");
    } else {
        println!("  auth-center:      {}", config.base_url);
        println!("  admin resource:   {}", config.admin_resource);
        println!("  every call is checked against <resource>:<action>");
    }
    println!("---------------------");
}

fn serve(disable_api_key_check: bool, port: u16) -> Result<()> {
    // All three variables are required (R6): with no local key table, an
    // instance missing any of them cannot authenticate anyone, so refuse to
    // start rather than come up half-configured. The one exception is a local
    // server started with --disable-api-key-check, which never introspects
    // anything; the bypass there is explicit in authz, never a consequence of
    // configuration having gone missing.
    let config = match AuthCenterConfig::from_env() {
        Ok(config) => config,
        Err(_) if disable_api_key_check => AuthCenterConfig::unusable(),
        Err(error) => return Err(error),
    };
    print_auth_summary(&config, disable_api_key_check);
    auth_center::install(AuthCenter::new(config));

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
        Commands::DebugParseConfig { file } => debug_parse_config(&file),
    }
}
