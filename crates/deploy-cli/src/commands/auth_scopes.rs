//! `deploy auth-scopes` — print the auth-center scopes a project needs.
//!
//! The reason this exists rather than a line in the docs: auth-center's
//! `validate_scope` rejects a three-segment scope and *suggests a fix* by
//! joining all but the last segment with `-`. So a hand-written
//! `deploy:hotlaps-staging:deploy` comes back with `deploy-hotlaps-staging:deploy`,
//! which is valid, mints cleanly, and names a resource this server will never
//! ask about — a key that is denied on every call with nothing to point at.
//! Generating the string removes the chance to take that bait.

use std::path::Path;

use anyhow::{bail, Result};
use deploy_core::config;
use deploy_core::rpc::*;

/// One action, the scope it produces on this resource, and the RPC methods it
/// covers. Derived from [`METHOD_TABLE`] so it cannot drift from what the
/// server actually authorizes against.
pub struct ActionScope {
    pub action: Action,
    pub scope: String,
    pub methods: Vec<&'static str>,
}

/// The scopes a key needs to work on one project's resource, in the order the
/// method table lists them.
///
/// `InstanceAdministration` methods are deliberately excluded: they are checked
/// against this instance's admin resource (`DEPLOY_ADMIN_RESOURCE`), not
/// against any project's, so putting them on a project key would grant nothing.
pub fn project_scopes(resource_name: &str) -> Vec<ActionScope> {
    let mut out: Vec<ActionScope> = Vec::new();

    for spec in METHOD_TABLE {
        if spec.resolution == ProjectResolution::InstanceAdministration {
            continue;
        }
        match out.iter_mut().find(|entry| entry.action == spec.action) {
            Some(entry) => entry.methods.push(spec.name),
            None => out.push(ActionScope {
                action: spec.action,
                scope: scope_string(resource_name, spec.action),
                methods: vec![spec.name],
            }),
        }
    }

    out
}

/// Shared with `create-project`, so both commands hand out the same wording and
/// the same generated commands.
pub fn print_scope_report(project_name: &str, resource_name: &str) {
    let scopes = project_scopes(resource_name);

    println!("Scopes for project '{project_name}' on resource '{resource_name}':");
    println!();
    for entry in &scopes {
        println!("  {}", entry.scope);
        println!("      {}", entry.methods.join(", "));
    }
    println!();

    // Resource names are global across auth-service projects, so a deploy
    // resource belongs to exactly one: the project that owns the thing being
    // deployed.
    println!(
        "Declare these in the auth-service project that owns '{resource_name}' — the project\n\
         that owns the thing being deployed. Resource names are globally unique across\n\
         auth-service projects, so a deploy resource must be declared in exactly one."
    );
    println!();
    print_auth_setup_commands(resource_name, &scopes);

    println!();
    println!(
        "Note: `deploy create-project` is not authorized against '{resource_name}'. It is\n\
         checked against the instance's administration resource (DEPLOY_ADMIN_RESOURCE),\n\
         as <admin-resource>:{}.",
        Action::CreateProject.as_str()
    );
}

/// Prints ready-to-run `auth-setup` lines. `auth-setup` holds no credential —
/// it proposes the change and blocks on a browser approval — so these are safe
/// to paste as-is.
pub fn print_auth_setup_commands(resource_name: &str, scopes: &[ActionScope]) {
    let role = format!("{resource_name}-deployer");

    println!("  auth-setup create-role {role} \\");
    println!("      --project <auth-service-project-id> \\");
    for entry in scopes {
        println!("      --scope {} \\", entry.scope);
    }
    println!("      --description 'Deploy access to {resource_name}'");
    println!();
    println!("  auth-setup create-key {resource_name}-key \\");
    println!("      --project <auth-service-project-id> \\");
    println!("      --role {role}");
}

/// Contacts no server and needs no API key: everything printed is derived from
/// the config file and the compiled-in method table.
pub fn auth_scopes(config_file: &Path, resource_name: &str) -> Result<()> {
    // Same check, and the same wording, as create-project: a colon here is the
    // three-segment scope that no key can hold.
    if resource_name.contains(':') {
        bail!(
            "--resource must not contain ':' (got {resource_name:?}).\n\
             A scope is <resource>:<action>, so grouping goes in the resource \
             name itself — \"do2-deploy\", not \"deploy:do2\"."
        );
    }

    let config_text = std::fs::read_to_string(config_file)?;
    let settings = config::parse_client_settings(&config_text);
    let project_name = settings.project_name.as_deref().unwrap_or("(unset)");

    println!();
    println!("Config:      {}", config_file.display());
    println!(
        "Destination: {}",
        settings.dest_url.as_deref().unwrap_or("(unset)")
    );
    println!();
    print_scope_report(project_name, resource_name);
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_exactly_what_scope_string_produces_for_each_action() {
        let scopes = project_scopes("hotlaps-api-staging");
        let produced: Vec<&str> = scopes.iter().map(|entry| entry.scope.as_str()).collect();
        assert_eq!(
            produced,
            vec![
                "hotlaps-api-staging:deploy",
                "hotlaps-api-staging:read",
                "hotlaps-api-staging:execute-sql",
                "hotlaps-api-staging:rollback",
            ]
        );
        for entry in &scopes {
            assert_eq!(
                entry.scope,
                scope_string("hotlaps-api-staging", entry.action)
            );
        }
    }

    #[test]
    fn every_project_method_in_the_table_is_covered_exactly_once() {
        let scopes = project_scopes("r");
        let listed: Vec<&str> = scopes
            .iter()
            .flat_map(|entry| entry.methods.iter().copied())
            .collect();
        let expected: Vec<&str> = METHOD_TABLE
            .iter()
            .filter(|spec| spec.resolution != ProjectResolution::InstanceAdministration)
            .map(|spec| spec.name)
            .collect();
        assert_eq!(listed, expected);
    }

    /// create-project is gated on the instance's admin resource, so offering it
    /// as a project scope would mint a grant that authorizes nothing.
    #[test]
    fn create_project_is_not_a_project_scope() {
        let scopes = project_scopes("hotlaps-api-staging");
        assert!(scopes
            .iter()
            .all(|entry| entry.action != Action::CreateProject));
    }

    #[test]
    fn a_resource_containing_a_colon_is_rejected() {
        let err = auth_scopes(Path::new("unused.qc"), "deploy:hotlaps-staging").unwrap_err();
        assert!(err.to_string().contains("must not contain"));
    }
}
