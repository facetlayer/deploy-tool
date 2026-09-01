//! The dashboard's OAuth client against auth-center.
//!
//! This is a back-end-for-frontend: the browser never sees an access token.
//! The SPA is served from the same origin as these routes, sign-in is a
//! redirect to auth-center, and the callback exchanges the code here, on the
//! server, for a token that is filed against an HttpOnly session cookie.
//!
//! The alternative — a public PKCE client exchanging the code in JavaScript —
//! would put a token auth-center scoped to a real admin user into a place any
//! script on the page can read. For a dashboard whose whole job is to show
//! what is deployed, that trade buys nothing.

use anyhow::{anyhow, Context, Result};

use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

/// What the dashboard needs beyond the three authorization variables. All of
/// these are optional as a group: an instance that sets none simply serves no
/// dashboard, which is how every existing instance keeps starting unchanged.
#[derive(Clone, Debug)]
pub struct DashboardConfig {
    /// This server's own public origin, e.g. `https://deploy.apf1.dev`. The
    /// redirect URI is derived from it and must match what auth-center has
    /// registered for the client, exactly.
    pub public_url: String,
    pub client_id: String,
    pub client_secret: String,
}

impl DashboardConfig {
    /// `None` when none of the three are set — the dashboard is off. `Err`
    /// when some but not all are: that is a typo in an environment file, and
    /// silently serving no dashboard is the least helpful possible response.
    pub fn from_env() -> Result<Option<DashboardConfig>> {
        let public_url = non_empty("DEPLOY_PUBLIC_URL");
        let client_id = non_empty("DEPLOY_OAUTH_CLIENT_ID");
        let client_secret = non_empty("DEPLOY_OAUTH_CLIENT_SECRET");
        match (public_url, client_id, client_secret) {
            (None, None, None) => Ok(None),
            (Some(public_url), Some(client_id), Some(client_secret)) => Ok(Some(DashboardConfig {
                public_url: public_url.trim_end_matches('/').to_string(),
                client_id,
                client_secret,
            })),
            _ => Err(anyhow!(
                "the dashboard needs all of DEPLOY_PUBLIC_URL, DEPLOY_OAUTH_CLIENT_ID and \
                 DEPLOY_OAUTH_CLIENT_SECRET, or none of them. Set all three to serve the \
                 dashboard, or remove them all to run without it."
            )),
        }
    }

    pub fn redirect_uri(&self) -> String {
        format!("{}/oauth/callback", self.public_url)
    }
}

fn non_empty(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}

fn random_token() -> String {
    let bytes: [u8; 32] = rand::thread_rng().gen();
    hex::encode(bytes)
}

/// A PKCE pair. The dashboard is a confidential client and authenticates the
/// exchange with its secret, so this is belt and braces — but it also means the
/// same code path works if the client is ever re-registered as public.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn new_pkce() -> Pkce {
    let verifier = format!("{}{}", random_token(), random_token());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

pub fn new_state() -> String {
    random_token()
}

pub fn authorize_url(
    auth_base_url: &str,
    config: &DashboardConfig,
    admin_resource: &str,
    state: &str,
    challenge: &str,
) -> String {
    let enc = urlencoding::encode;
    format!(
        "{auth_base_url}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}\
         &state={}&scope={}&code_challenge={}&code_challenge_method=S256",
        enc(&config.client_id),
        enc(&config.redirect_uri()),
        enc(state),
        // The scope recorded on the token. The decision that matters is still
        // the introspection on every request, not this string.
        enc(&format!("{admin_resource}:admin-read")),
        enc(challenge),
    )
}

pub fn logout_url(auth_base_url: &str, config: &DashboardConfig) -> String {
    let enc = urlencoding::encode;
    format!(
        "{auth_base_url}/oauth/logout?client_id={}&post_logout_redirect_uri={}",
        enc(&config.client_id),
        enc(&format!("{}/", config.public_url)),
    )
}

pub struct TokenResponse {
    pub access_token: String,
    pub expires_at: i64,
}

/// Blocking: the caller keeps this off the async runtime's worker threads, as
/// with every other outbound call in this server.
pub fn exchange_code(
    auth_base_url: &str,
    config: &DashboardConfig,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse> {
    let response: serde_json::Value = ureq::post(&format!("{auth_base_url}/oauth/token"))
        .timeout(std::time::Duration::from_secs(10))
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &config.redirect_uri()),
            ("client_id", &config.client_id),
            ("client_secret", &config.client_secret),
            ("code_verifier", verifier),
        ])
        .context("auth-center refused the authorization code")?
        .into_json()
        .context("auth-center's token response was not readable JSON")?;

    let access_token = response
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("auth-center's token response carried no access_token"))?
        .to_string();
    let expires_in = response
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(TokenResponse {
        access_token,
        expires_at: now + expires_in,
    })
}

/// Ends the token at auth-center as well as locally. Best-effort: the local
/// session is already gone by the time this runs, so a failure here costs the
/// user nothing they can see.
pub fn revoke(auth_base_url: &str, config: &DashboardConfig, access_token: &str) {
    let _ = ureq::post(&format!("{auth_base_url}/oauth/revoke"))
        .timeout(std::time::Duration::from_secs(5))
        .send_form(&[
            ("token", access_token),
            ("client_id", &config.client_id),
            ("client_secret", &config.client_secret),
        ]);
}
