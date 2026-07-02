use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::state::AppState;

const SHARD_COUNT: usize = 16;

type ShardMap = HashMap<String, Vec<Instant>>;

#[derive(Debug)]
struct Shard {
    requests: Mutex<ShardMap>,
}

impl Shard {
    fn new() -> Self {
        Self {
            requests: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RateLimitState {
    shards: Arc<Vec<Shard>>,
    pub max_requests: u32,
    pub window_duration: Duration,
}

fn hash_ip(ip: &str) -> usize {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ip.hash(&mut hasher);
    hasher.finish() as usize % SHARD_COUNT
}

impl RateLimitState {
    pub fn new(max_requests: u32, window_duration: Duration) -> Self {
        let shards = (0..SHARD_COUNT)
            .map(|_| Shard::new())
            .collect::<Vec<_>>();
        Self {
            shards: Arc::new(shards),
            max_requests,
            window_duration,
        }
    }

    pub async fn check_rate_limit(&self, client_ip: &str) -> bool {
        let idx = hash_ip(client_ip);
        let shard = &self.shards[idx];
        let mut requests = shard.requests.lock().await;
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
        let now = Instant::now();
        let window_start = now - self.window_duration;

        for shard in self.shards.iter() {
            let mut requests = shard.requests.lock().await;
            requests.retain(|_ip, times| {
                times.retain(|&t| t >= window_start);
                !times.is_empty()
            });
        }
    }
}

pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    _headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(rate_limiter) = &state.rate_limit {
        let client_ip = addr.ip().to_string();
        if !rate_limiter.check_rate_limit(&client_ip).await {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    let response = next.run(request).await;
    Ok(response)
}
