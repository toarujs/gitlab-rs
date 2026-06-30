use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use super::state::AppState;

#[derive(Debug, Clone)]
pub struct RateLimitState {
    pub requests: Arc<RwLock<HashMap<String, Vec<Instant>>>>,
    pub max_requests: u32,
    pub window_duration: Duration,
}

impl RateLimitState {
    pub fn new(max_requests: u32, window_duration: Duration) -> Self {
        Self {
            requests: Arc::new(RwLock::new(HashMap::new())),
            max_requests,
            window_duration,
        }
    }

    pub async fn check_rate_limit(&self, client_ip: &str) -> bool {
        let mut requests = self.requests.write().await;
        let now = Instant::now();

        let entry = requests
            .entry(client_ip.to_string())
            .or_insert_with(Vec::new);

        let window_start = now - self.window_duration;
        entry.retain(|&time| time >= window_start);

        if entry.len() >= self.max_requests as usize {
            return false;
        }

        entry.push(now);

        true
    }

    pub async fn cleanup_expired(&self) {
        let mut requests = self.requests.write().await;
        let now = Instant::now();
        let window_start = now - self.window_duration;
        requests.retain(|_ip, times| {
            times.retain(|&t| t >= window_start);
            !times.is_empty()
        });
    }
}

pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Use actual TCP connection IP, not X-Forwarded-For
    let client_ip = addr.ip().to_string();

    if let Some(rate_limiter) = &state.rate_limit {
        if !rate_limiter.check_rate_limit(&client_ip).await {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    let response = next.run(request).await;

    Ok(response)
}
