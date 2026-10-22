//! The `deploy` CLI. Port of src/cli.ts (yargs) and src/client/*.ts.

mod api_key;
mod client_setup;
mod commands;
mod detect_coding_agent;
mod git_tags;
mod rpc_client;
mod shell;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "deploy",
    version,
    about = "Deploy a project to a deploy-server instance"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Trigger a deployment using the specified configuration file(s)
    Run {
        /// Path to the deployment configuration file(s). Multiple files are
        /// deployed serially.
        #[arg(required = true)]
        config_file: Vec<PathBuf>,

        /// Override the destination URL from the config file
        #[arg(long)]
        override_dest: Option<String>,
    },

    /// Preview deployment drift: which files would be uploaded or deleted
    Preview {
        config_file: PathBuf,

        #[arg(long)]
        override_dest: Option<String>,
    },

    /// Show the local files that would be included in a deployment
    PreviewDeployFiles { config_file: PathBuf },

    /// Run a SQL query on a database in a deployed project
    Sql {
        config_file: PathBuf,

        /// SQL query to execute
        sql: String,

        /// Explicit database file path (relative to project dir) to use
        #[arg(long)]
        database: Option<String>,

        /// Output results as JSON (an array of row objects) instead of a table
        #[arg(long)]
        json: bool,

        #[arg(long)]
        override_dest: Option<String>,
    },

    /// List the databases configured for a deployed project
    ListDatabases {
        config_file: PathBuf,

        #[arg(long)]
        override_dest: Option<String>,
    },

    /// Run one of the project's allowed-server-side-scripts in its active
    /// deployment, and print its stdout and stderr
    ///
    /// Everything after the script path is passed to it unchanged, flags
    /// included. Put deploy's own options before the config file, and use `--`
    /// when the script's first argument is a flag deploy also has (`--help`).
    RunOnServer {
        config_file: PathBuf,

        /// Script path relative to the deployment, as listed in
        /// allowed-server-side-scripts
        script: String,

        /// Arguments for the script, passed as-is with no shell
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,

        #[arg(long)]
        override_dest: Option<String>,
    },

    /// Roll back a project to a previous deployment
    Rollback {
        config_file: PathBuf,

        /// Name of the deployment to roll back to (omit to choose interactively)
        deploy_name: Option<String>,

        #[arg(long)]
        override_dest: Option<String>,

        /// Number of recent deployments to list
        #[arg(long, default_value_t = 10)]
        limit: i64,
    },

    /// Show deployment history and the current active deployment
    History {
        config_file: PathBuf,

        #[arg(long)]
        override_dest: Option<String>,

        /// Number of recent deployments to show
        #[arg(long, default_value_t = 10)]
        limit: i64,
    },

    /// Show the git commit recorded for a deployment on the server
    CheckDeployedCommit {
        config_file: PathBuf,

        /// Deployment to check (defaults to the active deployment)
        #[arg(long)]
        deploy_name: Option<String>,

        /// Output the result as JSON
        #[arg(long)]
        json: bool,

        #[arg(long)]
        override_dest: Option<String>,
    },

    /// Copy a file from the server back to the local filesystem
    CopyBack {
        config_file: PathBuf,

        /// Relative path of the file to copy back from the server
        filename: String,

        #[arg(long)]
        override_dest: Option<String>,
    },

    /// Print the auth-center scopes a project needs, and the auth-setup
    /// commands that create them
    AuthScopes {
        config_file: PathBuf,

        /// auth-center resource this project is bound to on the target instance
        #[arg(long)]
        resource: String,
    },

    /// Run `candle check-start` in each active deployment with a candle-config
    StartServices,

    /// Dry run of start-services: list the candle commands that would run
    PreviewStartServices,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            config_file,
            override_dest,
        } => {
            // Serial, and the first failure aborts: deploying the rest of a set
            // after one has failed leaves a half-updated system.
            let multiple = config_file.len() > 1;
            for path in &config_file {
                if multiple {
                    println!();
                    println!("=== Deploying: {} ===", path.display());
                }
                commands::run::run(path, override_dest.as_deref())?;
            }
            Ok(())
        }

        Command::Preview {
            config_file,
            override_dest,
        } => commands::preview::preview(&config_file, override_dest.as_deref()),

        Command::PreviewDeployFiles { config_file } => {
            commands::preview::preview_deploy_files(&config_file)
        }

        Command::Sql {
            config_file,
            sql,
            database,
            json,
            override_dest,
        } => commands::sql::run_sql(
            &config_file,
            &sql,
            database.as_deref(),
            json,
            override_dest.as_deref(),
        ),

        Command::ListDatabases {
            config_file,
            override_dest,
        } => commands::sql::list_databases(&config_file, override_dest.as_deref()),

        Command::RunOnServer {
            config_file,
            script,
            args,
            override_dest,
        } => {
            let status = commands::run_on_server::run_on_server(
                &config_file,
                &script,
                &args,
                override_dest.as_deref(),
            )?;
            // The script's own status, not just success/failure.
            if status != 0 {
                std::process::exit(i32::from(status));
            }
            Ok(())
        }

        Command::Rollback {
            config_file,
            deploy_name,
            override_dest,
            limit,
        } => commands::rollback::rollback(
            &config_file,
            deploy_name.as_deref(),
            override_dest.as_deref(),
            limit,
        ),

        Command::History {
            config_file,
            override_dest,
            limit,
        } => commands::history::history(&config_file, override_dest.as_deref(), limit),

        Command::CheckDeployedCommit {
            config_file,
            deploy_name,
            json,
            override_dest,
        } => commands::history::check_deployed_commit(
            &config_file,
            deploy_name.as_deref(),
            json,
            override_dest.as_deref(),
        ),

        Command::CopyBack {
            config_file,
            filename,
            override_dest,
        } => commands::copy_back::copy_back(&config_file, &filename, override_dest.as_deref()),

        Command::AuthScopes {
            config_file,
            resource,
        } => commands::auth_scopes::auth_scopes(&config_file, &resource),

        Command::StartServices => commands::start_services::start_services(),

        Command::PreviewStartServices => commands::start_services::preview_start_services(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_accepts_several_config_files() {
        let cli = Cli::parse_from([
            "deploy",
            "run",
            "a.qc",
            "b.qc",
            "--override-dest",
            "http://x",
        ]);
        match cli.command {
            Command::Run {
                config_file,
                override_dest,
            } => {
                assert_eq!(config_file.len(), 2);
                assert_eq!(override_dest.as_deref(), Some("http://x"));
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn auth_scopes_takes_a_config_file_and_a_resource() {
        let cli = Cli::parse_from([
            "deploy",
            "auth-scopes",
            "deploy.qc",
            "--resource",
            "hotlaps-api-staging",
        ]);
        match cli.command {
            Command::AuthScopes {
                config_file,
                resource,
            } => {
                assert_eq!(config_file.to_str(), Some("deploy.qc"));
                assert_eq!(resource, "hotlaps-api-staging");
            }
            _ => panic!("expected auth-scopes"),
        }
    }

    #[test]
    fn run_on_server_passes_everything_after_the_script_through() {
        let cli = Cli::parse_from([
            "deploy",
            "run-on-server",
            "--override-dest",
            "http://x",
            "deploy.qc",
            "bin/migrate",
            "--dry-run",
            "-v",
            "--override-dest",
            "not-ours",
            "two words",
        ]);
        match cli.command {
            Command::RunOnServer {
                config_file,
                script,
                args,
                override_dest,
            } => {
                assert_eq!(config_file.to_str(), Some("deploy.qc"));
                assert_eq!(script, "bin/migrate");
                assert_eq!(
                    args,
                    vec![
                        "--dry-run",
                        "-v",
                        "--override-dest",
                        "not-ours",
                        "two words"
                    ]
                );
                assert_eq!(override_dest.as_deref(), Some("http://x"));
            }
            _ => panic!("expected run-on-server"),
        }
    }

    /// A flag deploy itself knows, as the first argument after the script, is
    /// still deploy's; `--` hands it to the script instead.
    #[test]
    fn a_double_dash_hands_one_of_deploys_own_flags_to_the_script() {
        assert!(
            Cli::try_parse_from(["deploy", "run-on-server", "a.qc", "bin/x", "--help"]).is_err()
        );
        match Cli::parse_from([
            "deploy",
            "run-on-server",
            "a.qc",
            "bin/x",
            "--",
            "--override-dest",
            "y",
        ])
        .command
        {
            Command::RunOnServer {
                args,
                override_dest,
                ..
            } => {
                assert_eq!(args, vec!["--override-dest", "y"]);
                assert_eq!(override_dest, None);
            }
            _ => panic!("expected run-on-server"),
        }
    }

    #[test]
    fn run_on_server_accepts_no_arguments_and_a_double_dash() {
        match Cli::parse_from(["deploy", "run-on-server", "a.qc", "bin/x"]).command {
            Command::RunOnServer { args, .. } => assert!(args.is_empty()),
            _ => panic!("expected run-on-server"),
        }
        match Cli::parse_from(["deploy", "run-on-server", "a.qc", "bin/x", "--", "--help"]).command
        {
            Command::RunOnServer { args, .. } => assert_eq!(args, vec!["--help"]),
            _ => panic!("expected run-on-server"),
        }
    }

    #[test]
    fn history_and_rollback_default_to_ten_entries() {
        match Cli::parse_from(["deploy", "history", "a.qc"]).command {
            Command::History { limit, .. } => assert_eq!(limit, 10),
            _ => panic!("expected history"),
        }
        match Cli::parse_from(["deploy", "rollback", "a.qc"]).command {
            Command::Rollback {
                limit, deploy_name, ..
            } => {
                assert_eq!(limit, 10);
                assert!(deploy_name.is_none());
            }
            _ => panic!("expected rollback"),
        }
    }
}
