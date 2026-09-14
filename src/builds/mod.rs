#![allow(dead_code, unused_imports)]
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::redis::WatchKeyStatus;

pub const RUNNER_BUILD_QUEUE_PREFIX: &str = "runner:build_queue:";
pub const RUNNER_BUILD_QUEUE_HEADER_KEY: &str = "Gitlab-Ci-Builds-Polling";
pub const RUNNER_BUILD_QUEUE_HEADER_VALUE: &str = "yes";
pub const MAX_REGISTER_BODY_SIZE: usize = 32 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchKeyRequest {
    pub key: String,
    #[serde(default)]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRequest {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub last_update: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterAction {
    Proxy,
    NoContent,
}

pub fn runner_queue_key(token: &str) -> String {
    format!("{RUNNER_BUILD_QUEUE_PREFIX}{token}")
}

pub fn parse_runner_request(content_type: Option<&str>, body: &[u8]) -> Result<RunnerRequest, String> {
    if !is_application_json(content_type) {
        return Err("invalid content-type received".to_string());
    }
    serde_json::from_slice(body).map_err(|e| e.to_string())
}

pub fn is_application_json(content_type: Option<&str>) -> bool {
    let Some(raw) = content_type else {
        return false;
    };
    let mime = raw.split(';').next().unwrap_or(raw).trim();
    mime.eq_ignore_ascii_case("application/json")
}

pub fn should_watch(token: &str, last_update: &str, duration: Duration) -> bool {
    !duration.is_zero() && !token.is_empty() && !last_update.is_empty()
}

/// Axum has not written the response yet, so SeenChange can still proxy Rails
/// (Go workhorse returns 204 here because ResponseWriter may be stale).
pub fn action_for_watch(status: WatchKeyStatus) -> RegisterAction {
    match status {
        WatchKeyStatus::AlreadyChanged | WatchKeyStatus::SeenChange => RegisterAction::Proxy,
        WatchKeyStatus::Timeout | WatchKeyStatus::NoChange => RegisterAction::NoContent,
    }
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

    #[test]
    fn runner_queue_key_matches_ce_19_3_1() {
        assert_eq!(
            runner_queue_key("glrt-abc"),
            "runner:build_queue:glrt-abc"
        );
    }

    #[test]
    fn parse_runner_request_reads_token_and_last_update() {
        let body = br#"{"token":"glrt-abc","last_update":"xyz","info":{"name":"gitlab-runner"}}"#;
        let req = parse_runner_request(Some("application/json; charset=utf-8"), body).unwrap();
        assert_eq!(req.token, "glrt-abc");
        assert_eq!(req.last_update, "xyz");
    }

    #[test]
    fn parse_runner_request_rejects_non_json() {
        assert!(parse_runner_request(Some("text/plain"), b"{}").is_err());
        assert!(parse_runner_request(None, b"{}").is_err());
    }

    #[test]
    fn missing_values_skip_watch() {
        assert!(!should_watch("", "x", Duration::from_secs(50)));
        assert!(!should_watch("tok", "", Duration::from_secs(50)));
        assert!(!should_watch("tok", "x", Duration::ZERO));
        assert!(should_watch("tok", "x", Duration::from_secs(50)));
    }

    #[test]
    fn watch_status_maps_to_register_action() {
        assert_eq!(
            action_for_watch(WatchKeyStatus::AlreadyChanged),
            RegisterAction::Proxy
        );
        assert_eq!(
            action_for_watch(WatchKeyStatus::SeenChange),
            RegisterAction::Proxy
        );
        assert_eq!(
            action_for_watch(WatchKeyStatus::Timeout),
            RegisterAction::NoContent
        );
        assert_eq!(
            action_for_watch(WatchKeyStatus::NoChange),
            RegisterAction::NoContent
        );
    }
}
