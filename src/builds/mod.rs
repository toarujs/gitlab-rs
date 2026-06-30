#![allow(dead_code, unused_imports)]
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchKeyRequest {
    pub key: String,
    #[serde(default)]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRequest {
    pub token: String,
    pub last_update: String,
}

#[derive(Debug, Clone)]
pub struct WatchKeyHandler {
    pub redis_url: Option<String>,
    pub default_timeout: Duration,
}

impl WatchKeyHandler {
    pub fn new() -> Self {
        Self {
            redis_url: None,
            default_timeout: Duration::from_secs(60),
        }
    }

    pub fn with_redis_url(mut self, url: String) -> Self {
        self.redis_url = Some(url);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    pub fn is_redis_available(&self) -> bool {
        self.redis_url.is_some()
    }
}

impl Default for WatchKeyHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watch_key_handler_default() {
        let handler = WatchKeyHandler::new();
        assert!(!handler.is_redis_available());
        assert_eq!(handler.default_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_watch_key_handler_with_redis() {
        let handler = WatchKeyHandler::new()
            .with_redis_url("redis://localhost:6379".to_string());
        assert!(handler.is_redis_available());
    }
}
