//! Harness for the CLI's end-to-end suite.
//!
//! The server-side harness (temp state directory, stub auth-center, spawning
//! `deploy-server`) is shared verbatim with `crates/deploy-server/tests` rather
//! than copied: both suites need exactly the same instance, and two drifting
//! copies of a security harness is how the old tool ended up with two drifting
//! copies of everything else.

#![allow(dead_code)]

#[path = "../../../deploy-server/tests/common/mod.rs"]
pub mod server;

pub use server::*;

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct CliRun {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CliRun {
    pub fn expect_ok(self, what: &str) -> CliRun {
        assert!(
            self.success,
            "{what} should have succeeded.\nstdout:\n{}\nstderr:\n{}",
            self.stdout, self.stderr
        );
        self
    }

    pub fn expect_failure(self, what: &str) -> CliRun {
        assert!(
            !self.success,
            "{what} should have failed.\nstdout:\n{}\nstderr:\n{}",
            self.stdout, self.stderr
        );
        self
    }

    pub fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// A workspace for one CLI test: the project being deployed, plus a fake HOME.
///
/// HOME is redirected because `find_api_key` falls back to `~/secrets/deploy.env`,
/// and a suite that silently picked up the developer's real key would prove
/// nothing about authorization.
pub struct CliWorkspace {
    pub project_dir: PathBuf,
    pub home: PathBuf,
}

impl CliWorkspace {
    pub fn new(root: &TempRoot, fixture: &str, dest_url: &str) -> CliWorkspace {
        let project_dir = root.join("project");
        copy_fixture(fixture, &project_dir, dest_url);
        CliWorkspace {
            project_dir,
            home: root.mkdir("fake-home"),
        }
    }

    pub fn config(&self) -> PathBuf {
        self.project_dir.join("deploy.qc")
    }

    pub fn write(&self, rel_path: &str, contents: &[u8]) {
        write_file(&self.project_dir.join(rel_path), contents);
    }

    pub fn remove(&self, rel_path: &str) {
        std::fs::remove_file(self.project_dir.join(rel_path)).unwrap();
    }

    /// Runs the `deploy` binary with `api_key` presented as `DEPLOY_API_KEY`.
    pub fn run(&self, api_key: &str, args: &[&str]) -> CliRun {
        let output = Command::new(cli_binary())
            .args(args)
            .current_dir(&self.project_dir)
            .env("HOME", &self.home)
            .env("DEPLOY_API_KEY", api_key)
            .env_remove("GOOBERNETES_API_KEY")
            .env_remove("XDG_STATE_HOME")
            .env_remove("DEPLOY_STATE_DIR")
            .output()
            .expect("could not run the deploy CLI");

        CliRun {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }

    /// `deploy run` against this workspace's config file.
    pub fn deploy(&self, api_key: &str) -> CliRun {
        self.run(api_key, &["run", "deploy.qc"])
    }
}

pub fn path_str(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
