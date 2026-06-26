#![allow(dead_code)]
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub struct LoadShedder {
    pub enabled: Arc<AtomicBool>,
    pub max_connections: u32,
    pub active_requests: Arc<AtomicU64>,
}

impl LoadShedder {
    pub fn new(max_connections: u32) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            max_connections,
            active_requests: Arc::new(AtomicU64::new(0)),
        }
    }

    fn should_shed(&self) -> bool {
        if !self.enabled.load(Ordering::Relaxed) {
            return false;
        }
        let active = self.active_requests.load(Ordering::Relaxed);
        active >= self.max_connections as u64
    }

    fn increment_active(&self) {
        self.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    fn decrement_active(&self) {
        self.active_requests.fetch_sub(1, Ordering::Relaxed);
    }
}

static SHEDDER: std::sync::LazyLock<LoadShedder> =
    std::sync::LazyLock::new(|| LoadShedder::new(10000));

pub async fn load_shedding_middleware(
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if SHEDDER.should_shed() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    SHEDDER.increment_active();
    let response = next.run(request).await;
    SHEDDER.decrement_active();

    Ok(response)
}

#[derive(Debug, Clone)]
pub struct LoadShedState {
    pub shedder: LoadShedder,
}
