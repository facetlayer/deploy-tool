//! `deploy run-on-server` — run one of the project's
//! `allowed-server-side-scripts` in its active deployment and print what it
//! printed.
//!
//! The server is the authority on what may run: it checks the allowlist
//! recorded with the active deployment, not this config. The local check here
//! is so a typo or a not-yet-deployed entry fails before any network call, with
//! the list of what is allowed.

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use deploy_core::config::{normalize_script_path, parse_allowed_server_side_scripts};
use deploy_core::rpc::*;

use crate::client_setup::setup_client;

/// `timeout(1)`'s convention, so a wrapper script can tell "the script failed"
/// from "the server gave up on it".
const EXIT_TIMED_OUT: u8 = 124;

/// Returns the exit status `deploy` itself should exit with: the script's own.
pub fn run_on_server(
    config_file: &Path,
    script: &str,
    args: &[String],
    override_dest: Option<&str>,
) -> Result<u8> {
    let script = normalize_script_path(script)?;

    let params = RunOnServerParams {
        project_name: String::new(),
        script: script.clone(),
        args: args.to_vec(),
    };
    params.validate_args()?;

    let setup = setup_client(config_file, override_dest)?;

    let allowed = parse_allowed_server_side_scripts(&setup.config_text)
        .with_context(|| format!("In {}", config_file.display()))?;
    if !allowed.contains(&script) {
        return Err(anyhow!(
            "{script} is not listed in allowed-server-side-scripts in {}.\nAllowed: {}",
            config_file.display(),
            if allowed.is_empty() {
                "(none)".to_string()
            } else {
                allowed.join(", ")
            }
        ));
    }

    let result = setup.client.run_on_server(&RunOnServerParams {
        project_name: setup.project_name.clone(),
        ..params
    })?;

    print_result(&result)?;
    Ok(exit_status(&result))
}

fn print_result(result: &RunOnServerResult) -> Result<()> {
    // Written raw, so the output can be piped exactly as the script wrote it.
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(result.stdout.as_bytes())?;
    stdout.flush()?;

    let mut stderr = std::io::stderr().lock();
    stderr.write_all(result.stderr.as_bytes())?;
    if result.stdout_truncated {
        writeln!(stderr, "deploy: stdout was truncated by the server")?;
    }
    if result.stderr_truncated {
        writeln!(stderr, "deploy: stderr was truncated by the server")?;
    }
    if result.timed_out {
        writeln!(
            stderr,
            "deploy: {} timed out on the server and was killed",
            result.script
        )?;
    } else if let Some(signal) = result.signal {
        writeln!(
            stderr,
            "deploy: {} was killed by signal {signal}",
            result.script
        )?;
    }
    stderr.flush()?;
    Ok(())
}

/// The script's exit code, or the shell's `128 + signal` when it was killed.
fn exit_status(result: &RunOnServerResult) -> u8 {
    if result.timed_out {
        return EXIT_TIMED_OUT;
    }
    match (result.exit_code, result.signal) {
        (Some(code), _) => u8::try_from(code).unwrap_or(1),
        (None, Some(signal)) => u8::try_from(128 + signal).unwrap_or(1),
        (None, None) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(exit_code: Option<i32>, signal: Option<i32>, timed_out: bool) -> RunOnServerResult {
        RunOnServerResult {
            exit_code,
            signal,
            timed_out,
            ..Default::default()
        }
    }

    #[test]
    fn the_scripts_exit_code_becomes_ours() {
        assert_eq!(exit_status(&result(Some(0), None, false)), 0);
        assert_eq!(exit_status(&result(Some(3), None, false)), 3);
    }

    #[test]
    fn a_signal_maps_to_128_plus_the_signal_and_a_timeout_to_124() {
        assert_eq!(exit_status(&result(None, Some(15), false)), 143);
        assert_eq!(exit_status(&result(None, Some(9), true)), EXIT_TIMED_OUT);
        assert_eq!(exit_status(&result(None, None, false)), 1);
    }

    #[test]
    fn an_unsafe_script_path_fails_before_reading_the_config() {
        let err = run_on_server(Path::new("missing.qc"), "../x", &[], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("'..'"), "{err}");
    }
}
