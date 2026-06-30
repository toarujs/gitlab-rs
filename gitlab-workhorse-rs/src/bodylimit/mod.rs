#![allow(dead_code)]
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum BodyLimitMode {
    Disabled,
    Logging,
    Enforced,
}

#[derive(Debug, Clone)]
pub struct BodyLimitConfig {
    pub max_bytes: usize,
    pub mode: BodyLimitMode,
}

impl BodyLimitConfig {
    pub fn new(max_bytes: usize, mode: BodyLimitMode) -> Self {
        Self { max_bytes, mode }
    }
}

#[derive(Debug, Clone)]
pub struct BodyLimitState {
    pub limits: Arc<tokio::sync::RwLock<std::collections::HashMap<String, BodyLimitConfig>>>,
    pub default_config: BodyLimitConfig,
}

impl BodyLimitState {
    pub fn new(default_max_bytes: usize) -> Self {
        Self {
            limits: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            default_config: BodyLimitConfig::new(default_max_bytes, BodyLimitMode::Enforced),
        }
    }

    pub async fn set_route_limit(&self, route: &str, max_bytes: usize, mode: BodyLimitMode) {
        let mut limits = self.limits.write().await;
        limits.insert(route.to_string(), BodyLimitConfig::new(max_bytes, mode));
    }

    pub async fn get_limit(&self, route: &str) -> BodyLimitConfig {
        let limits = self.limits.read().await;
        limits.get(route).cloned().unwrap_or_else(|| self.default_config.clone())
    }
}

pub async fn body_limit_middleware(
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let headers = request.headers();

    let has_transfer_encoding = headers
        .get("transfer-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("chunked"))
        .unwrap_or(false);

    let content_length = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok());

    // Per RFC 7230: if Transfer-Encoding is present, Content-Length must be ignored
    if has_transfer_encoding && content_length.is_some() {
        tracing::warn!(
            "Request has both Transfer-Encoding and Content-Length headers, rejecting. Path: {}",
            request.uri().path()
        );
        return Err(StatusCode::BAD_REQUEST);
    }

    // Track actual body size for chunked transfer
    if has_transfer_encoding {
        return Ok(next.run(request).await);
    }

    let size = content_length.unwrap_or(0);

    if size > 50 * 1024 * 1024 {
        tracing::warn!(
            "Request body too large: {} bytes, path: {}",
            size,
            request.uri().path()
        );
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    Ok(next.run(request).await)
}
