//! JSON-RPC 2.0 client. Port of src/client/rpc-client.ts.
//!
//! Transport is a plain HTTP POST of a single JSON-RPC request to
//! `{destUrl}/json-rpc`, authenticated with an `x-api-key` header. See
//! docs/ClientServerAPI.md in the old repo for the method surface.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use deploy_core::rpc::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

/// The server's body limit is 50MB; this is only the point at which a request
/// is worth complaining about, matching the old client's warning.
const LARGE_REQUEST_WARNING_BYTES: usize = 512 * 1024;

/// Uploads of large files can sit for a while on a slow link, and the deploy is
/// worthless if the connection is dropped halfway through the file set.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct RpcClient {
    agent: ureq::Agent,
    http_url: String,
    api_key: Option<String>,
    /// Shared so that clones handed to upload threads keep issuing distinct
    /// JSON-RPC ids.
    next_id: Arc<AtomicI64>,
}

impl RpcClient {
    pub fn new(dest_url: &str) -> RpcClient {
        RpcClient {
            agent: ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build(),
            http_url: json_rpc_url(dest_url),
            api_key: None,
            next_id: Arc::new(AtomicI64::new(1)),
        }
    }

    pub fn set_api_key(&mut self, api_key: impl Into<String>) {
        self.api_key = Some(api_key.into());
    }

    /// Sends one request and returns the deserialized `result`.
    pub fn call<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: &P) -> Result<R> {
        let value = self.call_raw(method, params)?;
        serde_json::from_value(value)
            .with_context(|| format!("JSON-RPC {method}: could not read the response"))
    }

    /// Sends one request whose `result` is not used. Several methods answer
    /// with `null` or an empty body, so the result is deliberately unparsed.
    pub fn notify<P: Serialize>(&self, method: &str, params: &P) -> Result<()> {
        self.call_raw(method, params)?;
        Ok(())
    }

    fn call_raw<P: Serialize>(&self, method: &str, params: &P) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let body = serde_json::to_string(&request)?;

        if body.len() > LARGE_REQUEST_WARNING_BYTES {
            eprintln!(
                "DeployRPCClient: Large request body (method: {}, size: {})",
                method,
                body.len()
            );
        }

        let mut http_request = self
            .agent
            .post(&self.http_url)
            .set("content-type", "application/json");

        if let Some(api_key) = &self.api_key {
            http_request = http_request.set("x-api-key", api_key);
        }

        let response = match http_request.send_string(&body) {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                // The server puts the useful part of a 4xx (bad API key, denied
                // authorization) in the body, so surface it rather than just
                // the status line.
                let text = response.into_string().unwrap_or_default();
                bail!(http_error_message(method, code, &text));
            }
            Err(err) => {
                return Err(anyhow!(err)).with_context(|| {
                    format!("JSON-RPC {method}: request to {} failed", self.http_url)
                })
            }
        };

        let text = response
            .into_string()
            .with_context(|| format!("JSON-RPC {method}: could not read the response body"))?;

        // A void method may answer with an empty body rather than a JSON-RPC
        // envelope.
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }

        let envelope: Value = serde_json::from_str(&text)
            .with_context(|| format!("JSON-RPC {method}: response was not JSON: {text}"))?;

        if let Some(error) = envelope.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            let code = error.get("code").and_then(Value::as_i64);
            let data = error
                .get("data")
                .map(|d| format!(" ({d})"))
                .unwrap_or_default();
            bail!(
                "JSON-RPC {method} failed{}: {message}{data}",
                code.map(|c| format!(" (code {c})")).unwrap_or_default()
            );
        }

        Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
    }

    // -----------------------------------------------------------------------
    // Typed method wrappers
    // -----------------------------------------------------------------------

    pub fn create_deployment(
        &self,
        params: &CreateDeploymentParams,
    ) -> Result<CreateDeploymentResult> {
        self.call(methods::CREATE_DEPLOYMENT, params)
    }

    pub fn add_manifest_files(&self, params: &AddManifestFilesParams) -> Result<()> {
        self.notify(methods::ADD_MANIFEST_FILES, params)
    }

    pub fn finalize_manifest(&self, params: &FinalizeManifestParams) -> Result<()> {
        self.notify(methods::FINALIZE_MANIFEST, params)
    }

    pub fn get_needed_files(&self, params: &GetNeededFilesParams) -> Result<GetNeededFilesResult> {
        self.call(methods::GET_NEEDED_FILES, params)
    }

    pub fn upload_one_file(&self, params: &UploadOneFileParams) -> Result<()> {
        self.notify(methods::UPLOAD_ONE_FILE, params)
    }

    pub fn start_multi_part_upload(&self, params: &StartMultiPartUploadParams) -> Result<()> {
        self.notify(methods::START_MULTIPART_UPLOAD, params)
    }

    pub fn upload_file_part(&self, params: &UploadFilePartParams) -> Result<()> {
        self.notify(methods::UPLOAD_FILE_PART, params)
    }

    pub fn finish_multi_part_upload(&self, params: &FinishMultiPartUploadParams) -> Result<()> {
        self.notify(methods::FINISH_MULTIPART_UPLOAD, params)
    }

    pub fn finish_uploads(&self, params: &FinishUploadsParams) -> Result<()> {
        self.notify(methods::FINISH_UPLOADS, params)
    }

    pub fn verify_deployment(
        &self,
        params: &VerifyDeploymentParams,
    ) -> Result<VerifyDeploymentResult> {
        self.call(methods::VERIFY_DEPLOYMENT, params)
    }

    pub fn activate_deployment(&self, params: &ActivateDeploymentParams) -> Result<()> {
        self.notify(methods::ACTIVATE_DEPLOYMENT, params)
    }

    pub fn preview_deployment(
        &self,
        params: &PreviewDeploymentParams,
    ) -> Result<PreviewDeploymentResult> {
        self.call(methods::PREVIEW_DEPLOYMENT, params)
    }

    pub fn preview_by_deploy_name(
        &self,
        params: &PreviewByDeployNameParams,
    ) -> Result<PreviewDeploymentResult> {
        self.call(methods::PREVIEW_BY_DEPLOY_NAME, params)
    }

    pub fn download_file(&self, params: &DownloadFileParams) -> Result<DownloadFileResult> {
        self.call(methods::DOWNLOAD_FILE, params)
    }

    pub fn execute_sql(&self, params: &ExecuteSqlParams) -> Result<ExecuteSqlResult> {
        self.call(methods::EXECUTE_SQL, params)
    }

    pub fn list_databases(&self, params: &ListDatabasesParams) -> Result<ListDatabasesResult> {
        self.call(methods::LIST_DATABASES, params)
    }

    pub fn list_deployments(
        &self,
        params: &ListDeploymentsParams,
    ) -> Result<ListDeploymentsResult> {
        self.call(methods::LIST_DEPLOYMENTS, params)
    }

    pub fn rollback(&self, params: &RollbackParams) -> Result<()> {
        self.notify(methods::ROLLBACK, params)
    }

    pub fn get_deployment_tags(
        &self,
        params: &GetDeploymentTagsParams,
    ) -> Result<GetDeploymentTagsResult> {
        self.call(methods::GET_DEPLOYMENT_TAGS, params)
    }

    pub fn create_project(&self, params: &CreateProjectParams) -> Result<CreateProjectResult> {
        self.call(methods::CREATE_PROJECT, params)
    }
}

/// An authorization denial arrives as HTTP 401 whose body is still a JSON-RPC
/// error envelope, so read it as one rather than showing the user raw JSON.
/// When the server judged the key active it names the scope it checked in
/// `data`, which is what turns a mistyped resource into an obvious message
/// instead of a silent wall of denials.
fn http_error_message(method: &str, code: u16, text: &str) -> String {
    let text = text.trim();

    if code == 401 {
        if let Some(error) = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|envelope| envelope.get("error").cloned())
        {
            let detail = error
                .get("data")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "unauthorized".to_string());
            return format!("JSON-RPC {method} denied: {detail}");
        }
    }

    format!(
        "JSON-RPC {method} failed: HTTP {code}{}",
        if text.is_empty() {
            String::new()
        } else {
            format!(": {text}")
        }
    )
}

/// Configs carry a bare origin (`https://do2.example`), so the endpoint path is
/// appended unless it is already there.
fn json_rpc_url(dest_url: &str) -> String {
    let mut url = dest_url.to_string();
    if !url.ends_with("/json-rpc") {
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str("json-rpc");
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_the_endpoint_path() {
        assert_eq!(
            json_rpc_url("https://do2.example"),
            "https://do2.example/json-rpc"
        );
        assert_eq!(
            json_rpc_url("https://do2.example/"),
            "https://do2.example/json-rpc"
        );
    }

    #[test]
    fn a_denial_reads_as_the_scope_that_was_checked() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32001,
                       "message":"Unauthorized",
                       "data":"this key does not hold hotlaps-api-staging:deploy"}}"#;
        assert_eq!(
            http_error_message("createDeployment", 401, body),
            "JSON-RPC createDeployment denied: \
             this key does not hold hotlaps-api-staging:deploy"
        );
    }

    /// A denial with nothing to disclose still reads as a sentence.
    #[test]
    fn a_denial_without_data_falls_back_to_the_message() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"Unauthorized"}}"#;
        assert_eq!(
            http_error_message("createDeployment", 401, body),
            "JSON-RPC createDeployment denied: Unauthorized"
        );
    }

    #[test]
    fn other_statuses_keep_the_raw_body() {
        assert_eq!(
            http_error_message("executeSql", 500, "boom"),
            "JSON-RPC executeSql failed: HTTP 500: boom"
        );
        assert_eq!(
            http_error_message("executeSql", 413, ""),
            "JSON-RPC executeSql failed: HTTP 413"
        );
    }

    #[test]
    fn leaves_an_explicit_endpoint_alone() {
        assert_eq!(
            json_rpc_url("https://do2.example/json-rpc"),
            "https://do2.example/json-rpc"
        );
    }
}
