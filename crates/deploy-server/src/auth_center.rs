//! HTTP client for auth-center's introspection endpoint.
//!
//! Ported from `~/tools/deploy/rust/src/auth_service.rs`, with its central bug
//! removed. The old file did:
//!
//! ```ignore
//! let allowed = match payload.get("allowed") {
//!     None | Some(Value::Null) => true,
//!     Some(value) => value.as_bool() == Some(true),
//! };
//! ```
//!
//! and only sent a scope when the RPC params happened to carry `projectName`.
//! Most methods — the whole upload path plus `activateDeployment` — carry only
//! a `deployName`, so no scope went out, no `allowed` came back, and every
//! active key from every project was accepted. That is Defect 1 in
//! `~/auth-center/docs/deploy-service-requirements.md`.
//!
//! Here a scope is always sent, and a response without an explicit
//! `allowed: true` is a denial.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

/// How long a successful introspection is trusted (R5). Revocation latency is
/// bounded by this, so keep it short.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Give up (and deny) if auth-center takes longer than this (R4).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Safety valve so a key-scanning attacker cannot grow the map without bound.
const MAX_CACHE_ENTRIES: usize = 1000;

/// The auth-center key that authorized a call. Recorded against the deployment
/// so `deploy history` can answer "who shipped this" (R7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyIdentity {
    pub key_id: String,
    pub name: Option<String>,
}

/// Result of one introspection.
///
/// `Denied` and `Unavailable` both deny; they are distinct so an operator can
/// tell "your key is wrong" from "auth-center is down" (R4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Introspection {
    Allowed(KeyIdentity),
    /// auth-center answered, and the answer was no.
    Denied {
        detail: String,
    },
    /// auth-center could not be asked, or its answer could not be read.
    Unavailable {
        detail: String,
    },
}

/// Outcome of the best-effort resource existence probe used by `createProject`.
/// See "Known gap" in docs/auth-integration.md.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceCheck {
    /// auth-center confirmed the resource exists.
    Verified,
    /// auth-center answered that this resource does not exist. Reject.
    NotFound,
    /// No usable answer — most likely because auth-center has no resource
    /// registry yet. Proceed, loudly.
    Unverifiable { detail: String },
}

/// Per-instance auth-center configuration, read from the environment file.
#[derive(Clone, Debug)]
pub struct AuthCenterConfig {
    /// Base URL with any trailing slash removed. Never hardcoded: do2 and dohl
    /// each get their own setting and their own service key.
    pub base_url: String,
    /// This instance's own key, which must hold `auth:introspect`.
    pub service_key: String,
    /// D2: the resource naming this instance for administration actions. There
    /// is deliberately no default and no derivation from the hostname.
    pub admin_resource: String,
}

impl AuthCenterConfig {
    /// Reads the three variables together. All are required: with no local key
    /// table left (R6), an instance missing any of them cannot authenticate
    /// anyone, so `Err` is fatal — starting half-configured is a security
    /// problem, not a degraded mode.
    pub fn from_env() -> Result<AuthCenterConfig> {
        let base_url =
            non_empty_env("DEPLOY_AUTH_URL").ok_or_else(|| missing("DEPLOY_AUTH_URL"))?;
        let service_key =
            non_empty_env("DEPLOY_AUTH_KEY").ok_or_else(|| missing("DEPLOY_AUTH_KEY"))?;
        let admin_resource = non_empty_env("DEPLOY_ADMIN_RESOURCE")
            .ok_or_else(|| missing("DEPLOY_ADMIN_RESOURCE"))?;

        Ok(AuthCenterConfig {
            base_url: base_url.trim_end_matches('/').to_string(),
            service_key,
            admin_resource,
        })
    }
}

fn missing(name: &str) -> anyhow::Error {
    anyhow!("{name} is not set; refusing to start. All of DEPLOY_AUTH_URL, DEPLOY_AUTH_KEY and DEPLOY_ADMIN_RESOURCE are required.")
}

fn non_empty_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}

struct CacheEntry {
    expires_at: Instant,
    identity: KeyIdentity,
}

pub struct AuthCenter {
    config: AuthCenterConfig,
    agent: ureq::Agent,
    /// Positive verdicts only (R5). A denial is always re-asked, so revoking a
    /// key takes effect immediately for calls it was already being refused.
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl AuthCenter {
    pub fn new(config: AuthCenterConfig) -> AuthCenter {
        AuthCenter::with_timeout(config, REQUEST_TIMEOUT)
    }

    /// Timeout is injectable so the tests can exercise the timeout path without
    /// spending five seconds doing it.
    pub fn with_timeout(config: AuthCenterConfig, timeout: Duration) -> AuthCenter {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout(timeout)
            .build();

        AuthCenter {
            config,
            agent,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn admin_resource(&self) -> &str {
        &self.config.admin_resource
    }

    /// Asks auth-center whether `presented_key` holds
    /// `deploy:<resource>:<action>`.
    ///
    /// Accepts only on 2xx AND `active == true` AND `allowed == true`.
    /// Everything else denies: network error, timeout, non-2xx, a missing
    /// field, or an unparseable body (R4).
    pub fn introspect(
        &self,
        presented_key: &str,
        resource: &str,
        action: deploy_core::rpc::Action,
    ) -> Introspection {
        let scope = deploy_core::rpc::scope_string(resource, action);
        let cache_key = cache_key_for(presented_key, resource, action);
        let now = Instant::now();

        if let Some(identity) = self.cache_get(&cache_key, now) {
            return Introspection::Allowed(identity);
        }

        let url = format!("{}/api/v1/introspect", self.config.base_url);
        let response = self
            .agent
            .post(&url)
            .set("content-type", "application/json")
            .set(
                "authorization",
                &format!("Bearer {}", self.config.service_key),
            )
            .send_json(serde_json::json!({ "token": presented_key, "scope": scope }));

        let payload: Json = match response {
            Ok(response) => match response.into_json() {
                Ok(value) => value,
                Err(err) => {
                    return self.unavailable(format!(
                        "auth-center {url} returned an unparseable body: {err}"
                    ))
                }
            },
            Err(ureq::Error::Status(code, response)) => {
                let detail = truncated_body(response);
                return self.unavailable(format!(
                    "auth-center {url} returned HTTP {code} for scope '{scope}': {detail}"
                ));
            }
            Err(err) => {
                return self.unavailable(format!("auth-center request to {url} failed: {err}"));
            }
        };

        // Both fields must be present and literally true. A response missing
        // `allowed` is a denial, not a pass — that is the bug this rewrite
        // exists to remove.
        let active = payload.get("active").and_then(Json::as_bool) == Some(true);
        let allowed = payload.get("allowed").and_then(Json::as_bool) == Some(true);

        if !active || !allowed {
            return Introspection::Denied {
                detail: format!(
                    "auth-center denied scope '{scope}': active={} allowed={}",
                    payload.get("active").unwrap_or(&Json::Null),
                    payload.get("allowed").unwrap_or(&Json::Null),
                ),
            };
        }

        // key_id is what attribution is recorded under (R7). auth-center always
        // sends it for an active key; fall back rather than fail the call.
        let identity = KeyIdentity {
            key_id: payload
                .get("key_id")
                .and_then(Json::as_str)
                .unwrap_or("unknown")
                .to_string(),
            name: payload
                .get("name")
                .and_then(Json::as_str)
                .map(str::to_string),
        };

        self.cache_put(cache_key, now, identity.clone());
        Introspection::Allowed(identity)
    }

    /// Best-effort check that auth-center knows the named resource, so a typo
    /// in `create-project --resource` surfaces at registration rather than as a
    /// mass denial at the next deploy (R1).
    ///
    /// auth-center has no resource registry today, so in practice every call
    /// takes the `Unverifiable` branch. See "Known gap" in
    /// docs/auth-integration.md; this is a stub only in the typo check, never in
    /// the authorization path.
    ///
    /// Unused until `createProject` wires it up; kept here because the client is
    /// where the three-branch behavior is specified.
    #[allow(dead_code)]
    pub fn verify_resource_exists(&self, resource: &str) -> ResourceCheck {
        let url = format!(
            "{}/api/v1/resources/{}",
            self.config.base_url,
            urlencode_segment(resource)
        );

        match self
            .agent
            .get(&url)
            .set(
                "authorization",
                &format!("Bearer {}", self.config.service_key),
            )
            .call()
        {
            Ok(_) => ResourceCheck::Verified,
            Err(ureq::Error::Status(404, response)) => {
                // Distinguishing "no such resource" from "no such endpoint"
                // matters: the first must reject the registration, the second
                // must not. auth-center's API errors are JSON objects; a route
                // that does not exist answers with an empty or non-JSON body.
                let body = truncated_body(response);
                if serde_json::from_str::<Json>(&body)
                    .ok()
                    .filter(|value| value.is_object())
                    .is_some()
                {
                    ResourceCheck::NotFound
                } else {
                    ResourceCheck::Unverifiable {
                        detail: format!(
                            "{url} answered 404 with no JSON body; \
                             auth-center probably has no resource registry"
                        ),
                    }
                }
            }
            Err(ureq::Error::Status(code, _)) => ResourceCheck::Unverifiable {
                detail: format!("{url} answered HTTP {code}"),
            },
            Err(err) => ResourceCheck::Unverifiable {
                detail: format!("{url} could not be reached: {err}"),
            },
        }
    }

    /// Logged here rather than at the call site: an operator reading the
    /// journal needs to tell "your key is wrong" from "auth-center is down",
    /// and only this layer knows which happened.
    fn unavailable(&self, detail: String) -> Introspection {
        eprintln!("[deploy error] denying, auth-center unavailable: {detail}");
        Introspection::Unavailable { detail }
    }

    fn cache_get(&self, cache_key: &str, now: Instant) -> Option<KeyIdentity> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(cache_key) {
            Some(entry) if entry.expires_at > now => Some(entry.identity.clone()),
            Some(_) => {
                cache.remove(cache_key);
                None
            }
            None => None,
        }
    }

    fn cache_put(&self, cache_key: String, now: Instant, identity: KeyIdentity) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= MAX_CACHE_ENTRIES {
            cache.retain(|_, entry| entry.expires_at > now);
            if cache.len() >= MAX_CACHE_ENTRIES {
                cache.clear();
            }
        }
        cache.insert(
            cache_key,
            CacheEntry {
                expires_at: now + CACHE_TTL,
                identity,
            },
        );
    }
}

/// R5: keyed by the key's hash *and* the resource *and* the action, so a
/// cached `read` verdict can never satisfy an `sql` call.
fn cache_key_for(presented_key: &str, resource: &str, action: deploy_core::rpc::Action) -> String {
    let mut hasher = Sha256::new();
    hasher.update(presented_key.as_bytes());
    format!(
        "{}|{}|{}",
        hex::encode(hasher.finalize()),
        resource,
        action.as_str()
    )
}

fn truncated_body(response: ureq::Response) -> String {
    response
        .into_string()
        .unwrap_or_default()
        .chars()
        .take(400)
        .collect()
}

/// Resource names come from an operator, but they land in a URL path, so
/// percent-encode anything outside the unreserved set rather than trusting them.
#[allow(dead_code)]
fn urlencode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The process-wide client, installed once at startup so the positive cache is
/// shared across requests.
static GLOBAL: OnceLock<AuthCenter> = OnceLock::new();

/// Installs the client for the lifetime of the process. Called once from
/// `serve`; later calls are ignored.
pub fn install(auth: AuthCenter) {
    let _ = GLOBAL.set(auth);
}

/// `None` only before `install`, which `serve` does before it binds a port.
pub fn global() -> Option<&'static AuthCenter> {
    GLOBAL.get()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use deploy_core::rpc::Action;

    #[test]
    fn scope_and_action_are_part_of_the_cache_key() {
        let a = cache_key_for("k", "hotlaps-staging", Action::Deploy);
        let b = cache_key_for("k", "hotlaps-staging", Action::Sql);
        let c = cache_key_for("k", "hotlaps-prod", Action::Deploy);
        let d = cache_key_for("other", "hotlaps-staging", Action::Deploy);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_eq!(a, cache_key_for("k", "hotlaps-staging", Action::Deploy));
    }

    #[test]
    fn cache_key_does_not_contain_the_key_text() {
        let key = cache_key_for("super-secret", "res", Action::Read);
        assert!(!key.contains("super-secret"));
    }

    #[test]
    fn url_segments_are_encoded() {
        assert_eq!(urlencode_segment("hotlaps-staging"), "hotlaps-staging");
        assert_eq!(urlencode_segment("a/b"), "a%2Fb");
        assert_eq!(urlencode_segment("a b"), "a%20b");
    }

    /// R6: there is no configuration in which the server runs without
    /// auth-center, so every missing variable is fatal rather than a fallback.
    #[test]
    fn every_missing_variable_refuses_to_start() {
        let complete = [
            ("DEPLOY_AUTH_URL", Some("https://auth.example")),
            ("DEPLOY_AUTH_KEY", Some("svc")),
            ("DEPLOY_ADMIN_RESOURCE", Some("deploy-do2")),
        ];

        for missing in 0..complete.len() {
            let mut env = complete;
            env[missing].1 = None;
            temp_env(&env, || {
                let err = AuthCenterConfig::from_env().unwrap_err().to_string();
                assert!(err.contains(complete[missing].0), "{err}");
            });
        }
    }

    #[test]
    fn trailing_slash_is_trimmed() {
        temp_env(
            &[
                ("DEPLOY_AUTH_URL", Some("https://auth.example/")),
                ("DEPLOY_AUTH_KEY", Some("svc")),
                ("DEPLOY_ADMIN_RESOURCE", Some("deploy-do2")),
            ],
            || {
                let config = AuthCenterConfig::from_env().unwrap();
                assert_eq!(config.base_url, "https://auth.example");
                assert_eq!(config.admin_resource, "deploy-do2");
            },
        );
    }

    // -- a stub auth-center ------------------------------------------------

    /// What the stub answers with. `delay` exists so the timeout path can be
    /// exercised without spending the real five seconds on it.
    pub(crate) struct StubReply {
        status: u16,
        body: String,
        delay: Duration,
    }

    impl StubReply {
        pub(crate) fn json(status: u16, body: &str) -> StubReply {
            StubReply {
                status,
                body: body.to_string(),
                delay: Duration::ZERO,
            }
        }

        pub(crate) fn after(mut self, delay: Duration) -> StubReply {
            self.delay = delay;
            self
        }
    }

    pub(crate) struct StubAuthCenter {
        pub(crate) base_url: String,
        requests: std::sync::Arc<Mutex<Vec<Json>>>,
    }

    impl StubAuthCenter {
        /// The request bodies received so far, in order. Introspect bodies are
        /// JSON; a GET arrives as `Json::Null`.
        pub(crate) fn requests(&self) -> Vec<Json> {
            self.requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    /// A single-threaded HTTP/1.1 stub. Hand-rolled rather than pulled from a
    /// crate so the tests can serve malformed bodies and stall on demand.
    ///
    /// `responder` receives the request index, the path, and the parsed body.
    pub(crate) fn start_stub(
        responder: impl Fn(usize, &str, &Json) -> StubReply + Send + 'static,
    ) -> StubAuthCenter {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = std::sync::Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();

        std::thread::spawn(move || {
            let mut index = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());

                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();

                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
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
                    continue;
                }
                let parsed: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);
                recorded
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(parsed.clone());

                let reply = responder(index, &path, &parsed);
                index += 1;
                if !reply.delay.is_zero() {
                    std::thread::sleep(reply.delay);
                }

                let response = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        StubAuthCenter {
            base_url: format!("http://127.0.0.1:{port}"),
            requests,
        }
    }

    #[test]
    fn stub_records_the_scope_that_was_asked_about() {
        let stub = start_stub(|_, _, _| {
            StubReply::json(
                200,
                r#"{"active":true,"allowed":true,"key_id":"k1","name":"ci"}"#,
            )
        });
        let auth = AuthCenter::with_timeout(
            AuthCenterConfig {
                base_url: stub.base_url.clone(),
                service_key: "svc".to_string(),
                admin_resource: "deploy-test".to_string(),
            },
            Duration::from_millis(500),
        );

        let outcome = auth.introspect("presented", "hotlaps-staging", Action::Deploy);
        assert_eq!(
            outcome,
            Introspection::Allowed(KeyIdentity {
                key_id: "k1".to_string(),
                name: Some("ci".to_string()),
            })
        );
        assert_eq!(stub.requests()[0]["scope"], "deploy:hotlaps-staging:deploy");
    }

    /// Environment variables are process-global, so serialize the tests that
    /// touch them and restore whatever was there before.
    pub(crate) fn temp_env(vars: &[(&str, Option<&str>)], body: impl FnOnce()) {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(name, _)| (name.to_string(), std::env::var(name).ok()))
            .collect();
        for (name, value) in vars {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(&name, value),
                None => std::env::remove_var(&name),
            }
        }
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}
