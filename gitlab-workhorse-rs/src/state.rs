#![allow(dead_code)]

use axum::{Json, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::cache;
use super::download;
use super::git;
use super::health;
use super::imageresizer;
use super::memory;
use super::metrics;
use super::proxy;
use super::ratelimit;
use super::secret;
use super::senddata;
use super::upload;

#[derive(Clone)]
pub struct AppState {
    pub proxy: proxy::ProxyState,
    pub upload: upload::UploadState,
    pub download: download::DownloadState,
    pub git: git::GitState,
    pub health: health::HealthState,
    pub metrics: metrics::MetricsState,
    pub rate_limit: Option<ratelimit::RateLimitState>,
    pub cache: Option<cache::CacheState>,
    pub memory_pool: memory::MemoryPool,
    pub injecters: Arc<senddata::InjecterRegistry>,
    pub secret: secret::Secret,
    pub webp_converter: Arc<imageresizer::WebPConverter>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("proxy", &self.proxy)
            .field("upload", &self.upload)
            .field("download", &self.download)
            .field("git", &self.git)
            .field("health", &self.health)
            .field("metrics", &self.metrics)
            .field("rate_limit", &self.rate_limit)
            .field("cache", &self.cache)
            .field("memory_pool", &self.memory_pool)
            .field("injecters", &"<InjecterRegistry>")
            .field("secret", &"<secret>")
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub build_time: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionResponse {
    pub version: String,
    pub build_time: String,
}

pub async fn health_check() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_time: chrono::Utc::now().to_rfc3339(),
    })
}

pub async fn version() -> impl IntoResponse {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_time: chrono::Utc::now().to_rfc3339(),
    })
}
