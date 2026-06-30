#![allow(dead_code)]

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

use super::state::AppState;

#[derive(Debug, Clone)]
pub struct HealthState {
    pub is_ready: Arc<RwLock<bool>>,
    pub is_alive: Arc<RwLock<bool>>,
    pub readiness_probe_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: HealthStatus,
    pub checks: Vec<HealthCheck>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
    Degraded,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: String,
    pub status: HealthStatus,
    pub message: Option<String>,
    pub duration_ms: u64,
}

impl HealthState {
    pub fn new(readiness_probe_url: Option<String>) -> Self {
        Self {
            is_ready: Arc::new(RwLock::new(false)),
            is_alive: Arc::new(RwLock::new(true)),
            readiness_probe_url,
        }
    }

    pub async fn set_ready(&self, ready: bool) {
        let mut is_ready = self.is_ready.write().await;
        *is_ready = ready;
    }

    pub async fn set_alive(&self, alive: bool) {
        let mut is_alive = self.is_alive.write().await;
        *is_alive = alive;
    }

    pub async fn is_ready(&self) -> bool {
        let is_ready = self.is_ready.read().await;
        *is_ready
    }

    pub async fn is_alive(&self) -> bool {
        let is_alive = self.is_alive.read().await;
        *is_alive
    }
}

pub async fn readiness_probe(
    State(state): State<AppState>,
) -> Result<Json<HealthResponse>, StatusCode> {
    let is_ready = state.health.is_ready().await;

    let status = if is_ready {
        HealthStatus::Healthy
    } else {
        HealthStatus::Unhealthy
    };

    let response = HealthResponse {
        status,
        checks: vec![HealthCheck {
            name: "readiness".to_string(),
            status: if is_ready {
                HealthStatus::Healthy
            } else {
                HealthStatus::Unhealthy
            },
            message: if is_ready {
                Some("Service is ready".to_string())
            } else {
                Some("Service is not ready".to_string())
            },
            duration_ms: 0,
        }],
        timestamp: chrono::Utc::now(),
    };

    if is_ready {
        Ok(Json(response))
    } else {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    }
}

pub async fn liveness_probe(
    State(state): State<AppState>,
) -> Result<Json<HealthResponse>, StatusCode> {
    let is_alive = state.health.is_alive().await;

    let status = if is_alive {
        HealthStatus::Healthy
    } else {
        HealthStatus::Unhealthy
    };

    let response = HealthResponse {
        status,
        checks: vec![HealthCheck {
            name: "liveness".to_string(),
            status: if is_alive {
                HealthStatus::Healthy
            } else {
                HealthStatus::Unhealthy
            },
            message: if is_alive {
                Some("Service is alive".to_string())
            } else {
                Some("Service is not alive".to_string())
            },
            duration_ms: 0,
        }],
        timestamp: chrono::Utc::now(),
    };

    if is_alive {
        Ok(Json(response))
    } else {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    }
}

pub async fn check_backend_health(url: &str) -> bool {
    let client = reqwest::Client::new();
    match client.get(url).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}
