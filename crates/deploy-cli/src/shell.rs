//! Running local shell hooks.
//!
//! The old client used `runShellCommand(cmd, [], { shell: true })`, so hook
//! text is a shell line and not an argv. Stdout and stderr are inherited: the
//! point of a `before-deploy` hook is to watch the build.

use std::path::Path;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result};

pub fn run_shell_command(command: &str, cwd: &Path) -> Result<ExitStatus> {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("Failed to run shell command in {}: {command}", cwd.display()))
}
