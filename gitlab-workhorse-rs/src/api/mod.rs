#![allow(dead_code, unused_imports)]
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::Response as AxumResponse,
};
use hyper::body::Incoming;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub mod block;
pub mod channel_settings;
pub mod gob_settings;

pub use channel_settings::ChannelSettings;
pub use gob_settings::GOBSettings;

pub const RESPONSE_CONTENT_TYPE: &str = "application/vnd.gitlab-workhorse+json";

pub const FAILURE_RESPONSE_LIMIT: usize = 32768;

#[derive(Clone)]
pub struct Api {
    pub url: Arc<Url>,
    pub version: String,
    pub client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitalyServer {
    pub address: String,
    pub token: String,
    #[serde(default)]
    pub call_metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitalyRepository {
    pub storage_name: String,
    pub relative_path: String,
    #[serde(rename = "git_object_directory", default)]
    pub git_object_directory: String,
    #[serde(rename = "git_alternate_object_directories", default)]
    pub git_alternate_object_directories: Vec<String>,
    #[serde(rename = "gl_repository", default)]
    pub gl_repository: String,
    #[serde(rename = "gl_project_path", default)]
    pub gl_project_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteObject {
    #[serde(rename = "GetURL", default)]
    pub get_url: String,
    #[serde(rename = "DeleteURL", default)]
    pub delete_url: String,
    #[serde(rename = "SkipDelete", default)]
    pub skip_delete: bool,
    #[serde(rename = "StoreURL", default)]
    pub store_url: String,
    #[serde(rename = "CustomPutHeaders", default)]
    pub custom_put_headers: bool,
    #[serde(rename = "PutHeaders", default)]
    pub put_headers: HashMap<String, String>,
    #[serde(rename = "UseWorkhorseClient", default)]
    pub use_workhorse_client: bool,
    #[serde(rename = "RemoteTempObjectID", default)]
    pub remote_temp_object_id: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub timeout: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(rename = "preApprovedTools", default)]
    pub pre_approved_tools: Option<Vec<String>>,
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuoWorkflowServiceConfig {
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub secure: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuoWorkflow {
    #[serde(rename = "Service", default)]
    pub service: Option<DuoWorkflowServiceConfig>,
    #[serde(rename = "CloudServiceForSelfHosted", default)]
    pub cloud_service_for_self_hosted: Option<DuoWorkflowServiceConfig>,
    #[serde(rename = "McpServers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(rename = "LockConcurrentFlow", default)]
    pub lock_concurrent_flow: bool,
    #[serde(rename = "ServerCapabilities", default)]
    pub server_capabilities: Vec<String>,
    #[serde(rename = "TimeoutHTTPRequests", default)]
    pub timeout_http_requests: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(rename = "GL_ID", default)]
    pub gl_id: String,
    #[serde(rename = "GL_USERNAME", default)]
    pub gl_username: String,
    #[serde(rename = "GL_REPOSITORY", default)]
    pub gl_repository: String,
    #[serde(rename = "GL_PROJECT_ID", default)]
    pub gl_project_id: i64,
    #[serde(rename = "GL_SCOPED_USER_ID", default)]
    pub gl_scoped_user_id: String,
    #[serde(rename = "GL_BUILD_ID", default)]
    pub gl_build_id: String,
    #[serde(rename = "ProjectID", default)]
    pub project_id: i64,
    #[serde(rename = "RootNamespaceID", default)]
    pub root_namespace_id: i64,
    #[serde(rename = "GitConfigOptions", default)]
    pub git_config_options: Vec<String>,
    #[serde(rename = "StoreLFSPath", default)]
    pub store_lfs_path: String,
    #[serde(rename = "LfsOid", default)]
    pub lfs_oid: String,
    #[serde(rename = "LfsSize", default)]
    pub lfs_size: i64,
    #[serde(rename = "TempPath", default)]
    pub temp_path: String,
    #[serde(rename = "RemoteObject", default)]
    pub remote_object: Option<RemoteObject>,
    #[serde(default)]
    pub archive: String,
    #[serde(default)]
    pub entry: String,
    #[serde(rename = "Channel", default)]
    pub channel: Option<ChannelSettings>,
    #[serde(rename = "GitalyServer", default)]
    pub gitaly_server: Option<GitalyServer>,
    #[serde(rename = "Repository", default)]
    pub repository: Option<GitalyRepository>,
    #[serde(rename = "ShowAllRefs", default)]
    pub show_all_refs: bool,
    #[serde(rename = "ProcessLsif", default)]
    pub process_lsif: bool,
    #[serde(rename = "MaximumSize", default)]
    pub maximum_size: i64,
    #[serde(rename = "UploadHashFunctions", default)]
    pub upload_hash_functions: Vec<String>,
    #[serde(rename = "NeedAudit", default)]
    pub need_audit: bool,
    #[serde(default)]
    pub gob: Option<GOBSettings>,
    #[serde(rename = "DuoWorkflow", default)]
    pub duo_workflow: Option<DuoWorkflow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoProxyEndpointResponse {
    pub geo_proxy_url: String,
    pub geo_proxy_extra_data: String,
    pub geo_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct GeoProxyData {
    pub geo_proxy_url: Option<Url>,
    pub geo_proxy_extra_data: String,
    pub geo_enabled: bool,
}

impl Api {
    pub fn new(url: Url, version: String, client: reqwest::Client) -> Self {
        Self {
            url: Arc::new(url),
            version,
            client,
        }
    }

    pub fn join_url(&self, suffix: &str) -> Url {
        let mut url = (*self.url).clone();
        let path = if url.path().ends_with('/') {
            format!("{}{}", url.path(), suffix.trim_start_matches('/'))
        } else {
            format!("{}/{}", url.path(), suffix.trim_start_matches('/'))
        };
        url.set_path(&path);
        url
    }

    pub async fn pre_authorize(
        &self,
        suffix: &str,
        method: &str,
        headers: &HeaderMap,
    ) -> Result<(StatusCode, HeaderMap, Option<Response>), String> {
        let url = self.join_url(suffix);

        let mut req = self.client.request(
            Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET),
            url.clone(),
        );

        for (key, value) in headers.iter() {
            if key.as_str().to_lowercase() != "host" {
                req = req.header(key.as_str(), value.as_bytes());
            }
        }

        let resp = req.send().await.map_err(|e| format!("request failed: {}", e))?;

        let status = resp.status();
        let mut response_headers = HeaderMap::new();
        for (key, value) in resp.headers() {
            if let Ok(name) = key.to_string().parse::<axum::http::HeaderName>() {
                if let Ok(val) = axum::http::HeaderValue::from_bytes(value.as_bytes()) {
                    response_headers.insert(name, val);
                }
            }
        }

        if status != StatusCode::OK || !is_valid_response_content_type(&response_headers) {
            return Ok((status, response_headers, None));
        }

        let body = resp.bytes().await.map_err(|e| format!("read body: {}", e))?;
        let auth_response: Response =
            serde_json::from_slice(&body).map_err(|e| format!("decode response: {}", e))?;

        Ok((status, response_headers, Some(auth_response)))
    }
}

fn is_valid_response_content_type(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with(RESPONSE_CONTENT_TYPE))
        .unwrap_or(false)
}

fn single_joining_slash(a: &str, b: &str) -> String {
    let a_slash = a.ends_with('/');
    let b_slash = b.starts_with('/');
    match (a_slash, b_slash) {
        (true, true) => format!("{}{}", a, &b[1..]),
        (false, false) => format!("{}/{}", a, b),
        _ => format!("{}{}", a, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_joining_slash() {
        assert_eq!(single_joining_slash("a/", "/b"), "a/b");
        assert_eq!(single_joining_slash("a", "b"), "a/b");
        assert_eq!(single_joining_slash("a/", "b"), "a/b");
        assert_eq!(single_joining_slash("a", "/b"), "a/b");
    }

    #[test]
    fn test_join_url() {
        let url = Url::parse("http://localhost:8080/api/v4").unwrap();
        let api = Api::new(url, "1.0".to_string(), reqwest::Client::new());
        let joined = api.join_url("internal/allowed");
        assert_eq!(joined.as_str(), "http://localhost:8080/api/v4/internal/allowed");
    }
}
