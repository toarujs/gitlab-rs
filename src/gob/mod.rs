#![allow(dead_code, unused_imports)]
use axum::{
    body::Body,
    extract::{OriginalUri, Request},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use regex::Regex;
use std::sync::LazyLock;
use url::Url;

use crate::api::{self, Response as ApiResponse};

static PROJECT_PATH_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/api/v4/projects/([^/]+)").unwrap());

const GOB_INTERNAL_PROJECT_AUTH_PATH: &str = "/api/v4/internal/observability/project/";

pub struct GobProxy {
    pub api_url: Option<Url>,
    pub version: String,
    pub development_mode: bool,
}

impl GobProxy {
    pub fn new(api_url: Option<Url>, version: String, development_mode: bool) -> Self {
        Self {
            api_url,
            version,
            development_mode,
        }
    }

    pub fn extract_project_id(path: &str) -> Option<String> {
        PROJECT_PATH_REGEX
            .captures(path)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }

    pub fn build_auth_path(project_id: &str, suffix: &str) -> String {
        format!("{}{}{}", GOB_INTERNAL_PROJECT_AUTH_PATH, project_id, suffix)
    }

    pub fn rewrite_path(original: &str) -> String {
        PROJECT_PATH_REGEX
            .replace(original, "")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_project_id() {
        let id = GobProxy::extract_project_id("/api/v4/projects/123/traces");
        assert_eq!(id.unwrap(), "123");
    }

    #[test]
    fn test_extract_project_id_no_match() {
        assert!(GobProxy::extract_project_id("/api/v4/users").is_none());
    }

    #[test]
    fn test_build_auth_path() {
        let path = GobProxy::build_auth_path("42", "/logs");
        assert_eq!(path, "/api/v4/internal/observability/project/42/logs");
    }

    #[test]
    fn test_rewrite_path() {
        let rewritten = GobProxy::rewrite_path("/api/v4/projects/123/traces");
        assert_eq!(rewritten, "/traces");
    }
}
