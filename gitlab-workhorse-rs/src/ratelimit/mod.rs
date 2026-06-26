use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use std::collections::HashMap;
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

        // Get or create entry for this client
        let entry = requests
            .entry(client_ip.to_string())
            .or_insert_with(Vec::new);

        // Remove old requests outside the window
        entry.retain(|&time| now.duration_since(time) < self.window_duration);

        // Check if limit exceeded
        if entry.len() >= self.max_requests as usize {
            return false;
        }

        // Add current request
        entry.push(now);

        true
    }
}

pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Get client IP from headers or use default
    let client_ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    // Check rate limit
    if let Some(rate_limiter) = &state.rate_limit {
        if !rate_limiter.check_rate_limit(&client_ip).await {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    // Process request
    let response = next.run(request).await;

    Ok(response)
}
