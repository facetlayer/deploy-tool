//! Harness for the end-to-end suite: a real `deploy-server` process on a temp
//! deployments directory and a temp database, talking to a stub auth-center.
//!
//! Everything here drives the server the way the CLI does — one JSON-RPC POST
//! at a time over HTTP — because the point of these tests is the seam between
//! the transport, the authorization decision and the handlers. A test that
//! called the handlers directly would skip the very layer that Defect 1 lived
//! in.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::{json, Value as Json};

// ---------------------------------------------------------------------------
// Temp directories
// ---------------------------------------------------------------------------

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// A throwaway directory tree. Kept on disk when `DEPLOY_TEST_KEEP=1`, so a
/// failing deployment test can be inspected after the fact.
pub struct TempRoot {
    pub path: PathBuf,
}

impl TempRoot {
    pub fn new(name: &str) -> TempRoot {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "deploy-tool-it-{}-{}-{}",
            name,
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        // macOS puts the temp dir behind a symlink, and the server canonicalizes
        // its deployments directory. Canonicalize here too so the paths a test
        // asserts on are the paths the server writes to.
        let path = path.canonicalize().unwrap();
        TempRoot { path }
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let dir = self.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if std::env::var("DEPLOY_TEST_KEEP").as_deref() == Ok("1") {
            eprintln!("[test] keeping {}", self.path.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

pub fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()))
}

/// Backdates a file so the `preserve-existing-files-max-age` sweep sees it as
/// stale. Shelling out to `touch` avoids pulling in a crate just to set an
/// mtime.
pub fn set_mtime_hours_ago(path: &Path, hours: i64) {
    let when = chrono::Local::now() - chrono::Duration::hours(hours);
    let stamp = when.format("%Y%m%d%H%M.%S").to_string();
    let status = Command::new("touch")
        .arg("-t")
        .arg(&stamp)
        .arg(path)
        .status()
        .expect("touch should be available");
    assert!(status.success(), "touch failed for {}", path.display());
}

pub fn sha_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Stub auth-center
// ---------------------------------------------------------------------------

pub struct StubReply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

impl StubReply {
    pub fn json(status: u16, body: impl Into<String>) -> StubReply {
        StubReply {
            status,
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn after(mut self, delay: Duration) -> StubReply {
        self.delay = delay;
        self
    }
}

pub type Responder = Arc<dyn Fn(&Json) -> StubReply + Send + Sync>;

pub struct StubAuthCenter {
    pub base_url: String,
    scopes_asked: Arc<Mutex<Vec<String>>>,
}

impl StubAuthCenter {
    /// Every `scope` this stub has been asked about, in order. Lets a test
    /// assert that a method reached auth-center at all, and with which scope.
    pub fn scopes_asked(&self) -> Vec<String> {
        self.scopes_asked.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.scopes_asked.lock().unwrap().clear();
    }
}

/// A thread-per-connection HTTP/1.1 stub. Hand-rolled so a test can answer with
/// a malformed body, a 500, or a stall — the three shapes R4 says must deny.
///
/// Thread-per-connection rather than a serial accept loop because the deploy
/// path uploads files in parallel, and a serial stub would serialize the whole
/// suite behind one introspection at a time.
pub fn start_stub_auth_center(responder: Responder) -> StubAuthCenter {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let scopes_asked = Arc::new(Mutex::new(Vec::new()));
    let recorded = scopes_asked.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let responder = responder.clone();
            let recorded = recorded.clone();
            std::thread::spawn(move || serve_one(stream, responder, recorded));
        }
    });

    StubAuthCenter {
        base_url: format!("http://127.0.0.1:{port}"),
        scopes_asked,
    }
}

fn serve_one(mut stream: TcpStream, responder: Responder, recorded: Arc<Mutex<Vec<String>>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return,
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
            .and_then(|v| v.parse::<usize>().ok())
        {
            content_length = value;
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body).is_err() {
        return;
    }

    let parsed: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);
    if let Some(scope) = parsed.get("scope").and_then(Json::as_str) {
        recorded.lock().unwrap().push(scope.to_string());
    }

    let reply = responder(&parsed);
    if !reply.delay.is_zero() {
        std::thread::sleep(reply.delay);
    }

    let response = format!(
        "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// The scope patterns one key holds, as they would be minted in the auth-center
/// dashboard.
pub struct KeyGrant {
    pub token: String,
    pub key_id: String,
    pub key_name: String,
    pub scopes: Vec<String>,
}

pub fn grant(token: &str, scopes: &[&str]) -> KeyGrant {
    KeyGrant {
        token: token.to_string(),
        key_id: format!("key_{token}"),
        key_name: token.to_string(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
    }
}

/// auth-center's matcher: `*` matches exactly one `:`-delimited segment, `**`
/// matches the rest. Reimplemented here so the suite checks against the real
/// semantics the scope format in docs/auth-integration.md relies on.
pub fn scope_matches(pattern: &str, scope: &str) -> bool {
    let pattern: Vec<&str> = pattern.split(':').collect();
    let scope: Vec<&str> = scope.split(':').collect();

    let mut p = 0;
    let mut s = 0;
    while p < pattern.len() {
        if pattern[p] == "**" {
            return true;
        }
        if s >= scope.len() {
            return false;
        }
        if pattern[p] != "*" && pattern[p] != scope[s] {
            return false;
        }
        p += 1;
        s += 1;
    }
    s == scope.len()
}

/// A stub that answers like a healthy auth-center: active keys get an explicit
/// `allowed`, unknown tokens get `{"active": false}`.
pub fn granting(grants: Vec<KeyGrant>) -> Responder {
    Arc::new(move |request: &Json| {
        let token = request.get("token").and_then(Json::as_str).unwrap_or("");
        let scope = request.get("scope").and_then(Json::as_str).unwrap_or("");

        match grants.iter().find(|g| g.token == token) {
            None => StubReply::json(200, r#"{"active":false}"#),
            Some(g) => {
                let allowed = g.scopes.iter().any(|p| scope_matches(p, scope));
                StubReply::json(
                    200,
                    json!({
                        "active": true,
                        "token_type": "api_key",
                        "key_id": g.key_id,
                        "name": g.key_name,
                        "scopes": g.scopes,
                        "allowed": allowed,
                        "scope": scope,
                    })
                    .to_string(),
                )
            }
        }
    })
}

// ---------------------------------------------------------------------------
// The server under test
// ---------------------------------------------------------------------------

pub struct ServerOptions {
    pub auth_url: String,
    pub auth_key: String,
    pub admin_resource: String,
    pub disable_api_key_check: bool,
    /// Extra environment for the server process, so a test can prove that a
    /// variable the server no longer reads has no effect.
    pub extra_env: Vec<(String, String)>,
}

impl Default for ServerOptions {
    fn default() -> ServerOptions {
        ServerOptions {
            // Port 1 is reserved and nothing listens there. The server requires
            // an auth-center URL to start at all, so the default is one that
            // denies everything by being unreachable (R4) rather than one that
            // quietly allows anything.
            auth_url: "http://127.0.0.1:1".to_string(),
            auth_key: "instance-service-key".to_string(),
            admin_resource: "deploy-test".to_string(),
            disable_api_key_check: false,
            extra_env: Vec::new(),
        }
    }
}

pub struct DeployServer {
    child: Child,
    pub port: u16,
    pub state_dir: PathBuf,
    pub deploys_dir: PathBuf,
    pub log_path: PathBuf,
}

/// Locates one of the workspace's binaries.
///
/// `CARGO_BIN_EXE_*` is only set for test targets in the package that owns the
/// binary, and this module is shared with `deploy-cli`'s suite (see the `#[path]`
/// include there), which needs both. The workspace shares one target directory,
/// so the fallback is the sibling of the directory holding this test binary.
fn workspace_binary(name: &str, compiled_in: Option<&'static str>) -> PathBuf {
    if let Some(path) = compiled_in {
        return PathBuf::from(path);
    }

    let test_exe = std::env::current_exe().expect("current_exe");
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/deps/<test>");
    let candidate = profile_dir.join(name);
    assert!(
        candidate.exists(),
        "{} has not been built. Run `cargo test --workspace`, or \
         `cargo build -p deploy-server` first.",
        candidate.display()
    );
    candidate
}

pub fn server_binary() -> PathBuf {
    workspace_binary("deploy-server", option_env!("CARGO_BIN_EXE_deploy-server"))
}

/// The `deploy` CLI, used by `deploy-cli`'s end-to-end suite.
pub fn cli_binary() -> PathBuf {
    workspace_binary("deploy", option_env!("CARGO_BIN_EXE_deploy"))
}

/// Ports this process has already handed to a server, so two instances in the
/// same test binary can never be given the same one.
static CLAIMED_PORTS: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// Grabs a port by binding and immediately releasing it.
///
/// There is an unavoidable race: another process — including a sibling test
/// binary that cargo is running in parallel — can take the port between the
/// release and the server's bind. `start_with_existing_state` retries rather
/// than pretending this cannot happen, because it does.
fn free_port() -> u16 {
    for _ in 0..100 {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut claimed = CLAIMED_PORTS.lock().unwrap();
        if !claimed.contains(&port) {
            claimed.push(port);
            return port;
        }
    }
    panic!("could not find an unclaimed port");
}

fn base_command(state_dir: &Path) -> Command {
    let mut command = Command::new(server_binary());
    command.env("DEPLOY_STATE_DIR", state_dir);
    // The suite must not inherit a developer's real instance configuration.
    for name in [
        "DEPLOY_AUTH_URL",
        "DEPLOY_AUTH_KEY",
        "DEPLOY_ADMIN_RESOURCE",
        "XDG_STATE_HOME",
    ] {
        command.env_remove(name);
    }
    command
}

impl DeployServer {
    /// Configures a state directory and a deployments directory, then starts
    /// `deploy-server serve` against them.
    pub fn start(root: &TempRoot, options: ServerOptions) -> DeployServer {
        let state_dir = root.mkdir("state");
        let deploys_dir = root.mkdir("deploys");

        let output = base_command(&state_dir)
            .arg("set-deployments-dir")
            .arg(&deploys_dir)
            .output()
            .expect("could not run set-deployments-dir");
        assert!(
            output.status.success(),
            "set-deployments-dir failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        Self::start_with_existing_state(root, state_dir, deploys_dir, options)
    }

    /// Starts another server process against a state directory that already
    /// exists — used to restart an instance with different environment.
    pub fn start_with_existing_state(
        root: &TempRoot,
        state_dir: PathBuf,
        deploys_dir: PathBuf,
        options: ServerOptions,
    ) -> DeployServer {
        // The port is chosen, released and then re-bound by the child, so a
        // parallel test binary can win the race. Losing it is cheap; retry on a
        // fresh port rather than failing a test for it.
        let mut last_failure = String::new();
        for _ in 0..5 {
            let port = free_port();
            let log_path = root.join(&format!("server-{port}.log"));
            let log = std::fs::File::create(&log_path).unwrap();

            let mut command = base_command(&state_dir);
            command.arg("serve").arg("--port").arg(port.to_string());
            if options.disable_api_key_check {
                command.arg("--disable-api-key-check");
            }
            // All three are required; the server refuses to start without them.
            command.env("DEPLOY_AUTH_URL", &options.auth_url);
            command.env("DEPLOY_AUTH_KEY", &options.auth_key);
            command.env("DEPLOY_ADMIN_RESOURCE", &options.admin_resource);
            for (name, value) in &options.extra_env {
                command.env(name, value);
            }

            let child = command
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .expect("could not start deploy-server");

            let mut server = DeployServer {
                child,
                port,
                state_dir: state_dir.clone(),
                deploys_dir: deploys_dir.clone(),
                log_path,
            };

            match server.wait_until_ready() {
                Ok(()) => return server,
                Err(reason) => {
                    server.stop();
                    last_failure = reason;
                }
            }
        }

        panic!("deploy-server would not start: {last_failure}");
    }

    /// Waits until the server answers JSON-RPC on its port.
    ///
    /// A bare TCP connect is not enough: when the port was taken between the
    /// pick and the bind, the child exits and the connect succeeds against
    /// whoever else is listening. Requiring a JSON-RPC envelope back proves the
    /// listener is this server.
    fn wait_until_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "the process exited with {status}. Log:\n{}",
                    self.logs()
                ));
            }
            if self.answers_json_rpc() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(format!(
            "no JSON-RPC response on port {} within 30s. Log:\n{}",
            self.port,
            self.logs()
        ))
    }

    fn answers_json_rpc(&self) -> bool {
        // No api key, so the answer is a 401 carrying a JSON-RPC error object —
        // which is exactly the proof that this is the deploy server.
        match Rpc::new(&self.url(), None).call("__probe__", json!({})) {
            Err(RpcError::Unauthorized) => true,
            Err(RpcError::Method(_)) => true,
            _ => false,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn logs(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    pub fn client(&self, api_key: &str) -> Rpc {
        Rpc::new(&self.url(), Some(api_key))
    }

    pub fn anonymous_client(&self) -> Rpc {
        Rpc::new(&self.url(), None)
    }

    pub fn db_path(&self) -> PathBuf {
        self.state_dir.join("db.sqlite")
    }

    /// Opens the instance's database directly. Used for the few assertions and
    /// fixtures that have no RPC surface, such as a project row with no
    /// resource binding.
    pub fn open_db(&self) -> Connection {
        let conn = Connection::open(self.db_path()).unwrap();
        conn.pragma_update(None, "busy_timeout", 5000).unwrap();
        conn
    }

    pub fn project_dir(&self, name: &str) -> PathBuf {
        self.deploys_dir.join(name)
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DeployServer {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC client
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum RpcError {
    /// The transport refused the call: HTTP 401 with the `-32001` code. This is
    /// what an authorization denial looks like on the wire.
    Unauthorized,
    /// The method ran and failed.
    Method(String),
    Transport(String),
}

impl RpcError {
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, RpcError::Unauthorized)
    }
}

pub struct Rpc {
    agent: ureq::Agent,
    url: String,
    api_key: Option<String>,
    next_id: AtomicUsize,
}

impl Rpc {
    pub fn new(dest_url: &str, api_key: Option<&str>) -> Rpc {
        Rpc {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(120))
                .build(),
            url: format!("{}/json-rpc", dest_url.trim_end_matches('/')),
            api_key: api_key.map(str::to_string),
            next_id: AtomicUsize::new(1),
        }
    }

    pub fn call(&self, method: &str, params: Json) -> Result<Json, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut http = self
            .agent
            .post(&self.url)
            .set("content-type", "application/json");
        if let Some(api_key) = &self.api_key {
            http = http.set("x-api-key", api_key);
        }

        let response = match http.send_string(&request.to_string()) {
            Ok(response) => response,
            Err(ureq::Error::Status(401, _)) => return Err(RpcError::Unauthorized),
            Err(err) => return Err(RpcError::Transport(err.to_string())),
        };

        let payload: Json = response
            .into_json()
            .map_err(|err| RpcError::Transport(err.to_string()))?;

        if let Some(error) = payload.get("error") {
            let message = error
                .get("message")
                .and_then(Json::as_str)
                .unwrap_or("(no message)")
                .to_string();
            return Err(RpcError::Method(message));
        }

        Ok(payload.get("result").cloned().unwrap_or(Json::Null))
    }

    /// Calls and unwraps, with the method name in the panic so a failure says
    /// which step of the deploy broke.
    pub fn ok(&self, method: &str, params: Json) -> Json {
        self.call(method, params)
            .unwrap_or_else(|err| panic!("{method} should have succeeded, got {err:?}"))
    }

    pub fn denied(&self, method: &str, params: Json) {
        match self.call(method, params) {
            Err(RpcError::Unauthorized) => {}
            other => panic!("{method} should have been denied, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Deployment helpers
// ---------------------------------------------------------------------------

/// One file as the CLI would present it: relative path plus contents.
pub struct SourceFile {
    pub rel_path: String,
    pub contents: Vec<u8>,
}

pub fn file(rel_path: &str, contents: &str) -> SourceFile {
    SourceFile {
        rel_path: rel_path.to_string(),
        contents: contents.as_bytes().to_vec(),
    }
}

pub fn binary_file(rel_path: &str, contents: Vec<u8>) -> SourceFile {
    SourceFile {
        rel_path: rel_path.to_string(),
        contents,
    }
}

/// Content big enough that its base64 encoding clears the client's 80KB
/// single-request ceiling, so the upload has to go multipart.
pub fn large_content(bytes: usize) -> Vec<u8> {
    (0..bytes).map(|i| b'a' + (i % 26) as u8).collect()
}

pub fn manifest_of(files: &[SourceFile]) -> Json {
    Json::Array(
        files
            .iter()
            .map(|f| json!({ "relPath": f.rel_path, "sha": sha_of(&f.contents) }))
            .collect(),
    )
}

/// The client's thresholds, from `deploy-cli`'s `run` module. Duplicated rather
/// than imported because the CLI is a binary crate; the constants are asserted
/// against each other in that crate's unit tests.
pub const MAX_REQUEST_SIZE_BYTES: usize = 80 * 1024;
pub const CHUNK_SIZE_BYTES: usize = MAX_REQUEST_SIZE_BYTES / 2;

/// Uploads one file, choosing the single-shot or the multipart path the same
/// way the CLI does.
pub fn upload(rpc: &Rpc, deploy_name: &str, source: &SourceFile) {
    let encoded = base64_of(&source.contents);

    if encoded.len() < MAX_REQUEST_SIZE_BYTES {
        rpc.ok(
            "uploadOneFile",
            json!({
                "deployName": deploy_name,
                "relPath": source.rel_path,
                "contentBase64": encoded,
            }),
        );
        return;
    }

    rpc.ok(
        "startMultiPartUpload",
        json!({ "deployName": deploy_name, "relPath": source.rel_path }),
    );

    let mut start = 0usize;
    while start < source.contents.len() {
        let end = (start + CHUNK_SIZE_BYTES).min(source.contents.len());
        rpc.ok(
            "uploadFilePart",
            json!({
                "deployName": deploy_name,
                "relPath": source.rel_path,
                "chunkStartsAt": start,
                "chunkBase64": base64_of(&source.contents[start..end]),
            }),
        );
        start = end;
    }

    rpc.ok(
        "finishMultiPartUpload",
        json!({ "deployName": deploy_name, "relPath": source.rel_path }),
    );
}

/// Drives one whole deployment: create, needed-files, upload, finish, verify,
/// activate. Returns the deploy name.
pub fn deploy(rpc: &Rpc, project: &str, config: &str, files: &[SourceFile]) -> String {
    let deploy_name = create_deployment(rpc, project, config, files);
    upload_needed(rpc, &deploy_name, files);
    finish_and_activate(rpc, &deploy_name);
    deploy_name
}

pub fn create_deployment(rpc: &Rpc, project: &str, config: &str, files: &[SourceFile]) -> String {
    let created = rpc.ok(
        "createDeployment",
        json!({
            "projectName": project,
            "sourceFileManifest": manifest_of(files),
            "sourceFileConfig": config,
        }),
    );
    created["deployName"].as_str().unwrap().to_string()
}

/// Asks for the needed files and uploads exactly those, so the dedup path is
/// exercised rather than bypassed.
pub fn upload_needed(rpc: &Rpc, deploy_name: &str, files: &[SourceFile]) -> Vec<String> {
    let needed = rpc.ok("getNeededFiles", json!({ "deployName": deploy_name }));
    let needed: Vec<String> = needed
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["relPath"].as_str().unwrap().to_string())
        .collect();

    for rel_path in &needed {
        let source = files
            .iter()
            .find(|f| &f.rel_path == rel_path)
            .unwrap_or_else(|| panic!("server asked for an unknown file: {rel_path}"));
        upload(rpc, deploy_name, source);
    }

    needed
}

pub fn finish_and_activate(rpc: &Rpc, deploy_name: &str) {
    rpc.ok("finishUploads", json!({ "deployName": deploy_name }));

    let verified = rpc.ok("verifyDeployment", json!({ "deployName": deploy_name }));
    assert_eq!(
        verified["status"], "success",
        "verification failed: {verified:?}"
    );

    rpc.ok("activateDeployment", json!({ "deployName": deploy_name }));
}

pub fn create_project(rpc: &Rpc, project: &str, resource: &str) {
    rpc.ok(
        "createProject",
        json!({ "projectName": project, "resourceName": resource }),
    );
}

/// A minimal `.qc` config. `extra` carries the directives under test —
/// `ignore`, `preserve-existing-files`, and so on.
pub fn config_for(project: &str, update_in_place: bool, extra: &str) -> String {
    let in_place = if update_in_place {
        "  update-in-place\n"
    } else {
        ""
    };
    format!(
        "deploy-settings\n  project-name={project}\n  dest-url=http://localhost:9999\n\
         {in_place}\ninclude **\n{extra}\n"
    )
}

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// The `tests/fixtures` directory at the repository root, shared by both
/// suites. Both crates live one level under `crates/`, so the relative path is
/// the same either way.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .expect("tests/fixtures should exist at the repository root")
}

/// Copies a fixture project into `dest` and points its `.qc` file at a server.
///
/// The destination URL is substituted rather than committed because the test
/// server's port is chosen at runtime.
pub fn copy_fixture(name: &str, dest: &Path, dest_url: &str) {
    copy_tree(&fixtures_dir().join(name), dest);

    let config = dest.join("deploy.qc");
    let text = read_file(&config).replace("__DEST_URL__", dest_url);
    std::fs::write(&config, text).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
