#![allow(dead_code, unused_imports)]
use axum::http::{HeaderMap, Method};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::api::RemoteObject;

const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryParams {
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub response_headers: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub upload_config: UploadConfig,
    #[serde(default)]
    pub ssrf_filter: bool,
    #[serde(default)]
    pub allow_localhost: bool,
    #[serde(default)]
    pub allowed_endpoints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UploadConfig {
    #[serde(default)]
    pub headers: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub authorized_upload_response: Option<AuthorizedUploadResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizedUploadResponse {
    #[serde(default)]
    pub temp_path: String,
    #[serde(default)]
    pub remote_object: Option<RemoteObject>,
    #[serde(default)]
    pub maximum_size: i64,
    #[serde(default)]
    pub upload_hash_functions: Vec<String>,
}

impl EntryParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.url.is_empty() {
            return Err("URL is required".to_string());
        }

        if !self.upload_config.method.is_empty() {
            let method = self.upload_config.method.to_uppercase();
            if method != "POST" && method != "PUT" {
                return Err(format!("invalid upload method: {}", self.upload_config.method));
            }
        }

        Ok(())
    }

    pub fn upload_method(&self) -> &str {
        if self.upload_config.method.is_empty() {
            "POST"
        } else {
            &self.upload_config.method
        }
    }
}

pub const DEPENDENCY_PROXY_PREFIX: &str = "send-dependency:";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_params_validate() {
        let params = EntryParams {
            url: "https://registry.example.com/v2/library/nginx/manifests/latest".to_string(),
            headers: HashMap::new(),
            response_headers: HashMap::new(),
            upload_config: UploadConfig::default(),
            ssrf_filter: true,
            allow_localhost: false,
            allowed_endpoints: Vec::new(),
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_entry_params_empty_url() {
        let params = EntryParams {
            url: "".to_string(),
            headers: HashMap::new(),
            response_headers: HashMap::new(),
            upload_config: UploadConfig::default(),
            ssrf_filter: false,
            allow_localhost: false,
            allowed_endpoints: Vec::new(),
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_upload_method_default() {
        let params = EntryParams {
            url: "test".to_string(),
            headers: HashMap::new(),
            response_headers: HashMap::new(),
            upload_config: UploadConfig::default(),
            ssrf_filter: false,
            allow_localhost: false,
            allowed_endpoints: Vec::new(),
        };
        assert_eq!(params.upload_method(), "POST");
    }
}
