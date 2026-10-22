//! `runOnServer`: start one of a project's `allowed-server-side-scripts` in its
//! active deployment and hand back what it printed.
//!
//! Authorization (`<resource>:run-on-server`) has already happened in the
//! transport. What this module adds is everything that keeps that grant from
//! meaning more than "run one of the listed files":
//!
//! - The allowlist comes from the config recorded with the active deployment,
//!   never from the request, so widening it takes a `deploy` key.
//! - The file is executed directly with the arguments as argv. There is no
//!   `sh -c` anywhere, so no argument is ever parsed as shell.
//! - The path is checked lexically, then again after symlinks are resolved: a
//!   deployed symlink pointing at `/bin/sh` is still outside the deployment.
//! - The child gets a scrubbed environment. The server's own environment holds
//!   `DEPLOY_AUTH_KEY` (which can introspect any key) and the dashboard's OAuth
//!   secret; a caller who may run scripts must not be able to print them.
//! - No stdin, a wall-clock limit, an output cap, and a cap on how many runs
//!   are in flight at once.
//! - Every run is recorded in `server_script_run` with the key that asked.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as Json;

use deploy_core::config::{normalize_script_path, parse_allowed_server_side_scripts};
use deploy_core::rpc::{RunOnServerParams, RunOnServerResult};

use crate::db;
use crate::handlers::parse_params;
use crate::paths::get_safe_path_in_dir;
use crate::state::{AppState, AuthorizedKey};

/// Below the CLI's 600s request timeout, so the caller gets a result that says
/// "timed out" rather than a dropped connection.
const RUN_TIMEOUT: Duration = Duration::from_secs(300);

/// Per stream. Output past this is read and discarded, so the child never
/// blocks on a full pipe.
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// How long to keep reading after the script exits. Anything still holding
/// its stdout past this is a background process it left behind.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Server-wide. A handler runs on a blocking-pool thread for the life of the
/// process, so without a cap a caller could tie up the pool.
const MAX_CONCURRENT_RUNS: usize = 4;

static RUNS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Environment variables passed through to the script. Everything else is
/// dropped; see the module doc for why.
const PASSTHROUGH_ENV: &[&str] = &["PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TZ"];

pub fn run_on_server(
    state: &AppState,
    params: &Json,
    authorized_by: Option<&AuthorizedKey>,
) -> Result<Json> {
    let params: RunOnServerParams = parse_params(params)?;
    params.validate_args()?;
    let script = normalize_script_path(&params.script)?;
    let project_name = params.project_name.as_str();

    let (deploy_name, deploy_dir, config_text) = {
        let conn = state.db();
        active_deployment(&conn, project_name)?
    };

    let allowed = parse_allowed_server_side_scripts(&config_text)
        .with_context(|| format!("The config of active deployment {deploy_name} is invalid"))?;
    if !allowed.contains(&script) {
        return Err(anyhow!(
            "{script} is not listed in allowed-server-side-scripts for the active deployment \
             of {project_name} ({deploy_name}). Allowed: {}",
            if allowed.is_empty() {
                "(none)".to_string()
            } else {
                allowed.join(", ")
            }
        ));
    }

    let executable = resolve_executable(&deploy_dir, &script)?;

    let _slot = RunSlot::acquire(&RUNS_IN_FLIGHT)?;

    let run_id = {
        let conn = state.db();
        record_start(
            &conn,
            project_name,
            &deploy_name,
            &script,
            &params.args,
            authorized_by,
        )?
    };

    eprintln!(
        "[deploy run-on-server] run {run_id}: project={project_name} deploy={deploy_name} \
         script={script} args={:?} key={}",
        params.args,
        describe_key(authorized_by),
    );

    // The database lock is not held while the process runs.
    let outcome = run_process(
        &executable,
        &params.args,
        &deploy_dir,
        &[
            ("DEPLOY_PROJECT_NAME", project_name),
            ("DEPLOY_NAME", deploy_name.as_str()),
        ],
        RUN_TIMEOUT,
    );

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!("[deploy run-on-server] run {run_id}: failed to start: {err:#}");
            let conn = state.db();
            let _ = record_finish(&conn, run_id, None, None, false);
            return Err(err);
        }
    };

    eprintln!(
        "[deploy run-on-server] run {run_id}: exit={:?} signal={:?} timed_out={}",
        outcome.exit_code, outcome.signal, outcome.timed_out
    );
    {
        let conn = state.db();
        record_finish(
            &conn,
            run_id,
            outcome.exit_code,
            outcome.signal,
            outcome.timed_out,
        )?;
    }

    Ok(serde_json::to_value(RunOnServerResult {
        script,
        deploy_name,
        exit_code: outcome.exit_code,
        signal: outcome.signal,
        timed_out: outcome.timed_out,
        stdout: String::from_utf8_lossy(&outcome.stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&outcome.stderr.bytes).into_owned(),
        stdout_truncated: outcome.stdout.truncated,
        stderr_truncated: outcome.stderr.truncated,
    })?)
}

/// The active deployment's name, directory and recorded config.
fn active_deployment(conn: &Connection, project_name: &str) -> Result<(String, PathBuf, String)> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "select d.deploy_name, d.deploy_dir, d.source_config_file
             from active_deployment a
             join deployment d on d.deploy_name = a.deploy_name
             where a.project_name = ?",
            [project_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let (deploy_name, deploy_dir, config) =
        row.ok_or_else(|| anyhow!("No active deployment found for project: {}", project_name))?;
    let config = config
        .ok_or_else(|| anyhow!("Deployment config not found for project: {}", project_name))?;
    let deploy_dir = db::get_deployments_dir(conn)?.join(deploy_dir);
    Ok((deploy_name, deploy_dir, config))
}

/// The lexical guard first, then the same check on the real path. Symlinks
/// are followed only to check where they land, and the resolved path is what
/// gets executed, so the file checked is the file run.
fn resolve_executable(deploy_dir: &Path, script: &str) -> Result<PathBuf> {
    let lexical = get_safe_path_in_dir(deploy_dir, script)?;

    let real_dir = std::fs::canonicalize(deploy_dir)
        .with_context(|| format!("Deployment directory is missing: {}", deploy_dir.display()))?;
    let real_path = std::fs::canonicalize(&lexical)
        .map_err(|_| anyhow!("{script} does not exist in the active deployment"))?;

    if !real_path.starts_with(&real_dir) {
        return Err(anyhow!(
            "{script} resolves to a file outside the deployment directory"
        ));
    }

    let meta = std::fs::metadata(&real_path)?;
    if !meta.is_file() {
        return Err(anyhow!("{script} is not a regular file"));
    }
    if !is_executable(&meta) {
        return Err(anyhow!(
            "{script} is not executable. Uploaded files keep no permission bits; add \
             `shell(chmod +x {script})` to after-deploy."
        ));
    }

    Ok(real_path)
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

fn describe_key(authorized_by: Option<&AuthorizedKey>) -> String {
    match authorized_by {
        Some(key) => match &key.key_name {
            Some(name) => format!("{} ({name})", key.key_id),
            None => key.key_id.clone(),
        },
        None => "(none)".to_string(),
    }
}

fn record_start(
    conn: &Connection,
    project_name: &str,
    deploy_name: &str,
    script: &str,
    args: &[String],
    authorized_by: Option<&AuthorizedKey>,
) -> Result<i64> {
    let (key_id, key_name) = match authorized_by {
        Some(key) => (Some(key.key_id.clone()), key.key_name.clone()),
        None => (None, None),
    };
    conn.execute(
        "insert into server_script_run (project_name, deploy_name, script, args_json,
            started_at, authorized_by_key_id, authorized_by_key_name)
         values (?, ?, ?, ?, ?, ?, ?)",
        params![
            project_name,
            deploy_name,
            script,
            serde_json::to_string(args)?,
            db::now_iso(),
            key_id,
            key_name,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn record_finish(
    conn: &Connection,
    run_id: i64,
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
) -> Result<()> {
    conn.execute(
        "update server_script_run
         set finished_at = ?, exit_code = ?, signal = ?, timed_out = ?
         where run_id = ?",
        params![db::now_iso(), exit_code, signal, timed_out, run_id],
    )?;
    Ok(())
}

/// Holds one of the [`MAX_CONCURRENT_RUNS`] slots until dropped. The counter
/// is a parameter only so tests can use their own.
struct RunSlot(&'static AtomicUsize);

impl RunSlot {
    fn acquire(counter: &'static AtomicUsize) -> Result<RunSlot> {
        let mut current = counter.load(Ordering::SeqCst);
        loop {
            if current >= MAX_CONCURRENT_RUNS {
                return Err(anyhow!(
                    "{current} server-side scripts are already running on this server; try again shortly"
                ));
            }
            match counter.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Ok(RunSlot(counter)),
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

struct ProcessOutcome {
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    stdout: CapturedStream,
    stderr: CapturedStream,
}

/// Runs `program` with `args` as argv — never through a shell — in `cwd`, with
/// only [`PASSTHROUGH_ENV`] plus `extra_env` in its environment.
fn run_process(
    program: &Path,
    args: &[String],
    cwd: &Path,
    extra_env: &[(&str, &str)],
    timeout: Duration,
) -> Result<ProcessOutcome> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in PASSTHROUGH_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in extra_env {
        command.env(name, value);
    }

    // Its own process group, so a timeout can kill whatever it spawned too.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to start {}", program.display()))?;

    let (done_tx, done_rx) = mpsc::channel::<()>();
    let stdout = capture(child.stdout.take(), done_tx.clone());
    let stderr = capture(child.stderr.take(), done_tx);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            kill_process_group(&mut child);
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    // Wait for both readers to hit EOF. If something the script left behind is
    // still holding a pipe open, stop waiting rather than hang the request; the
    // reader threads finish on their own when it lets go.
    let drain_deadline = Instant::now() + OUTPUT_DRAIN_GRACE;
    for _ in 0..2 {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(remaining).is_err() {
            break;
        }
    }

    let (exit_code, signal) = exit_parts(&status);
    Ok(ProcessOutcome {
        exit_code,
        signal,
        timed_out,
        stdout: take(&stdout),
        stderr: take(&stderr),
    })
}

/// Reads a pipe to EOF on its own thread, keeping the first
/// [`MAX_OUTPUT_BYTES`] and discarding the rest.
fn capture<R: Read + Send + 'static>(
    pipe: Option<R>,
    done: mpsc::Sender<()>,
) -> Arc<Mutex<CapturedStream>> {
    let buffer = Arc::new(Mutex::new(CapturedStream::default()));
    let Some(mut pipe) = pipe else {
        let _ = done.send(());
        return buffer;
    };
    let shared = Arc::clone(&buffer);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut captured = shared.lock().unwrap_or_else(|e| e.into_inner());
                    let room = MAX_OUTPUT_BYTES.saturating_sub(captured.bytes.len());
                    if n > room {
                        captured.truncated = true;
                    }
                    captured.bytes.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
        }
        let _ = done.send(());
    });
    buffer
}

fn take(buffer: &Arc<Mutex<CapturedStream>>) -> CapturedStream {
    std::mem::take(&mut *buffer.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(unix)]
fn kill_process_group(child: &mut Child) {
    // The child leads its own group (process_group(0)), so its pid is the
    // group id. Negative pid = the whole group.
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut Child) {
    let _ = child.kill();
}

#[cfg(unix)]
fn exit_parts(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    use std::os::unix::process::ExitStatusExt;
    (status.code(), status.signal())
}

#[cfg(not(unix))]
fn exit_parts(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deploy-run-on-server-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_timeout_kills_the_script_and_its_children() {
        let dir = temp("timeout");
        let path = script(
            &dir,
            "slow",
            "#!/bin/sh\nsleep 30 &\necho started\nsleep 30\n",
        );
        let started = Instant::now();
        let outcome = run_process(&path, &[], &dir, &[], Duration::from_millis(300)).unwrap();
        assert!(outcome.timed_out);
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.signal, Some(libc::SIGKILL));
        // The backgrounded `sleep` held stdout too; killing the group is what
        // lets the readers finish well inside the drain grace.
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(String::from_utf8_lossy(&outcome.stdout.bytes), "started\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_past_the_cap_is_truncated_without_blocking_the_child() {
        let dir = temp("cap");
        let path = script(
            &dir,
            "loud",
            &format!(
                "#!/bin/sh\nhead -c {} /dev/zero\n",
                MAX_OUTPUT_BYTES + 100_000
            ),
        );
        let outcome = run_process(&path, &[], &dir, &[], Duration::from_secs(20)).unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.stdout.truncated);
        assert_eq!(outcome.stdout.bytes.len(), MAX_OUTPUT_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_concurrency_cap_refuses_once_full_and_frees_on_drop() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let slots: Vec<RunSlot> = (0..MAX_CONCURRENT_RUNS)
            .map(|_| RunSlot::acquire(&COUNTER).unwrap())
            .collect();
        assert!(RunSlot::acquire(&COUNTER).is_err());
        drop(slots);
        assert_eq!(COUNTER.load(Ordering::SeqCst), 0);
        assert!(RunSlot::acquire(&COUNTER).is_ok());
    }
}
