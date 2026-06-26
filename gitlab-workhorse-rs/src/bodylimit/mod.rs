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
    let content_length = request
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > 50 * 1024 * 1024 {
        tracing::warn!(
            "Request body too large: {} bytes, path: {}",
            content_length,
            request.uri().path()
        );
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    Ok(next.run(request).await)
}
