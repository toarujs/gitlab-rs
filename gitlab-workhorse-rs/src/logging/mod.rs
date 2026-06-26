#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
}

impl RequestContext {
    pub fn new(request_id: String, method: String, path: String) -> Self {
        Self {
            request_id,
            method,
            path,
            remote_addr: None,
            user_agent: None,
        }
    }

    pub fn with_remote_addr(mut self, addr: String) -> Self {
        self.remote_addr = Some(addr);
        self
    }

    pub fn with_user_agent(mut self, agent: String) -> Self {
        self.user_agent = Some(agent);
        self
    }

    pub fn log_request(&self) {
        info!(
            request_id = %self.request_id,
            method = %self.method,
            path = %self.path,
            remote_addr = ?self.remote_addr,
            user_agent = ?self.user_agent,
            "Incoming request"
        );
    }

    pub fn log_response(&self, status: u16, duration_ms: u64) {
        info!(
            request_id = %self.request_id,
            method = %self.method,
            path = %self.path,
            status = status,
            duration_ms = duration_ms,
            "Request completed"
        );
    }

    pub fn log_error(&self, error: &str) {
        error!(
            request_id = %self.request_id,
            method = %self.method,
            path = %self.path,
            error = %error,
            "Request failed"
        );
    }
}

pub struct RequestTimer {
    start: Instant,
    request_id: String,
    method: String,
    path: String,
}

impl RequestTimer {
    pub fn new(request_id: String, method: String, path: String) -> Self {
        trace!(
            request_id = %request_id,
            method = %method,
            path = %path,
            "Request started"
        );

        Self {
            start: Instant::now(),
            request_id,
            method,
            path,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    pub fn finish(self, status: u16) {
        let duration_ms = self.elapsed_ms();

        if duration_ms > 1000 {
            warn!(
                request_id = %self.request_id,
                method = %self.method,
                path = %self.path,
                status = status,
                duration_ms = duration_ms,
                "Slow request detected"
            );
        } else {
            debug!(
                request_id = %self.request_id,
                method = %self.method,
                path = %self.path,
                status = status,
                duration_ms = duration_ms,
                "Request completed"
            );
        }
    }
}

impl Drop for RequestTimer {
    fn drop(&mut self) {
        // If finish wasn't called, log a warning
        if self.elapsed_ms() > 10000 {
            error!(
                request_id = %self.request_id,
                method = %self.method,
                path = %self.path,
                duration_ms = self.elapsed_ms(),
                "Request handler was dropped without completing"
            );
        }
    }
}

pub fn init_logging(log_level: &str, json_format: bool) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));

    if json_format {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_context() {
        let ctx = RequestContext::new(
            "test-123".to_string(),
            "GET".to_string(),
            "/api/test".to_string(),
        );

        assert_eq!(ctx.request_id, "test-123");
        assert_eq!(ctx.method, "GET");
        assert_eq!(ctx.path, "/api/test");
        assert!(ctx.remote_addr.is_none());
        assert!(ctx.user_agent.is_none());
    }

    #[test]
    fn test_request_context_with_remote_addr() {
        let ctx = RequestContext::new(
            "test-123".to_string(),
            "GET".to_string(),
            "/api/test".to_string(),
        )
        .with_remote_addr("127.0.0.1:12345".to_string());

        assert!(ctx.remote_addr.is_some());
    }

    #[test]
    fn test_request_timer() {
        let timer = RequestTimer::new(
            "test-123".to_string(),
            "GET".to_string(),
            "/api/test".to_string(),
        );

        assert!(timer.elapsed_ms() < 100);
    }
}
