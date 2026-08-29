//! `deploy create-project` — R1 of docs/auth-integration.md.
//!
//! New in this version, and a real behavior change: in the old tool a project
//! came into existence implicitly on first deploy. Now it is registered
//! explicitly and bound to an auth-center resource, so that the same project
//! name can require different resources on different instances (do2's
//! `hotlaps-api` binds to `hotlaps-staging`, dohl's to `hotlaps-prod`).

use anyhow::{anyhow, Result};
use deploy_core::rpc::*;

use crate::api_key::find_api_key;
use crate::rpc_client::RpcClient;

pub fn create_project(
    project_name: &str,
    resource_name: &str,
    rebind: bool,
    dest_url: Option<&str>,
) -> Result<()> {
    // There is no config file for this command — the project does not exist
    // yet — so the destination has to be given explicitly.
    let dest_url = dest_url.ok_or_else(|| {
        anyhow!("create-project needs a destination: pass --override-dest <url>")
    })?;

    let mut client = RpcClient::new(dest_url);
    match find_api_key(None) {
        Some(api_key) => client.set_api_key(api_key),
        None => eprintln!(
            "No API key found in DEPLOY_API_KEY environment variable or ~/secrets/deploy.env"
        ),
    }

    let result = client.create_project(&CreateProjectParams {
        project_name: project_name.to_string(),
        resource_name: resource_name.to_string(),
        rebind,
    })?;

    println!();
    match result.outcome {
        CreateProjectOutcome::Created => {
            println!("Registered project '{}' on {dest_url}", result.project_name);
        }
        CreateProjectOutcome::Rebound => {
            println!("Rebound project '{}' on {dest_url}", result.project_name);
            if let Some(previous) = &result.previous_resource_name {
                println!("  was bound to: {previous}");
            }
        }
        CreateProjectOutcome::Unchanged => {
            println!(
                "Project '{}' was already registered on {dest_url}",
                result.project_name
            );
        }
    }

    println!("  resource:     {}", result.resource_name);
    println!(
        "  deploy scope: {}",
        scope_string(&result.resource_name, Action::Deploy)
    );
    println!(
        "  keys for this project must hold deploy:{}:<action>, where <action> is one of \
         deploy, read, sql, rollback",
        result.resource_name
    );

    if !result.resource_verified {
        // auth-center has no resource registry yet, so a typo in the resource
        // name cannot be caught here — it shows up as every deploy being
        // denied. See "Known gap" in docs/auth-integration.md.
        println!();
        println!(
            "WARNING: the server could not verify that resource '{}' exists in auth-center.",
            result.resource_name
        );
        println!(
            "         Check the spelling against the auth-center dashboard: an unknown \
             resource denies every call for this project."
        );
    }

    println!();
    Ok(())
}
