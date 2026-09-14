#![allow(dead_code, unused_imports)]
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{error, info};

pub const NOTIFICATION_CHANNEL_PREFIX: &str = "workhorse:notifications:";

#[derive(Clone)]
pub struct RedisClient {
    manager: Arc<RwLock<Option<ConnectionManager>>>,
    url: String,
}

/// Matches CE 19.3.1 `workhorse/internal/redis.WatchKeyStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKeyStatus {
    Timeout,
    AlreadyChanged,
    SeenChange,
    NoChange,
}

pub struct KeyWatcher {
    client: RedisClient,
    key: String,
    current_value: Arc<RwLock<Option<String>>>,
}

impl RedisClient {
    pub fn new(url: String) -> Self {
        Self {
            manager: Arc::new(RwLock::new(None)),
            url,
        }
    }

    pub async fn connect(&self) -> Result<(), String> {
        let client = redis::Client::open(self.url.as_str())
            .map_err(|e| format!("redis client error: {}", e))?;

        let manager = ConnectionManager::new(client)
            .await
            .map_err(|e| format!("redis connection error: {}", e))?;

        let mut guard = self.manager.write().await;
        *guard = Some(manager);
        info!("Redis connected successfully");
        Ok(())
    }

    pub async fn is_connected(&self) -> bool {
        self.manager.read().await.is_some()
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), String> {
        let guard = self.manager.read().await;
        let manager = guard
            .as_ref()
            .ok_or("redis not connected")?;

        let mut conn = manager.clone();
        conn.set(key, value)
            .await
            .map_err(|e| format!("redis set error: {}", e))
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, String> {
        let guard = self.manager.read().await;
        let manager = guard
            .as_ref()
            .ok_or("redis not connected")?;

        let mut conn = manager.clone();
        conn.get(key)
            .await
            .map_err(|e| format!("redis get error: {}", e))
    }

    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or("redis not connected")?;
        let mut conn = manager.clone();
        conn.get(key)
            .await
            .map_err(|e| format!("redis get error: {}", e))
    }

    pub async fn del(&self, key: &str) -> Result<(), String> {
        let guard = self.manager.read().await;
        let manager = guard
            .as_ref()
            .ok_or("redis not connected")?;

        let mut conn = manager.clone();
        conn.del(key)
            .await
            .map_err(|e| format!("redis del error: {}", e))
    }

    pub async fn expire(&self, key: &str, seconds: i64) -> Result<(), String> {
        let guard = self.manager.read().await;
        let manager = guard
            .as_ref()
            .ok_or("redis not connected")?;

        let mut conn = manager.clone();
        redis::cmd("EXPIRE")
            .arg(key)
            .arg(seconds)
            .query_async(&mut conn)
            .await
            .map_err(|e| format!("redis expire error: {}", e))
    }

    pub async fn exists(&self, key: &str) -> Result<bool, String> {
        let guard = self.manager.read().await;
        let manager = guard
            .as_ref()
            .ok_or("redis not connected")?;

        let mut conn = manager.clone();
        conn.exists(key)
            .await
            .map_err(|e| format!("redis exists error: {}", e))
    }

    pub async fn incr(&self, key: &str) -> Result<i64, String> {
        let guard = self.manager.read().await;
        let manager = guard
            .as_ref()
            .ok_or("redis not connected")?;

        let mut conn = manager.clone();
        conn.incr(key, 1)
            .await
            .map_err(|e| format!("redis incr error: {}", e))
    }

    /// Subscribe first, then GET, then wait for PubSub — CE 19.3.1 KeyWatcher.WatchKey.
    pub async fn watch_key(
        &self,
        key: &str,
        value: &str,
        timeout: Duration,
    ) -> Result<WatchKeyStatus, String> {
        if !self.is_connected().await {
            return Err("redis not connected".to_string());
        }

        let client = redis::Client::open(self.url.as_str())
            .map_err(|e| format!("redis client error: {}", e))?;
        let mut pubsub = client
            .get_async_pubsub()
            .await
            .map_err(|e| format!("redis pubsub error: {}", e))?;
        let channel = notification_channel(key);
        pubsub
            .subscribe(&channel)
            .await
            .map_err(|e| format!("redis subscribe error: {}", e))?;

        let current = self.get(key).await?.unwrap_or_default();
        if current != value {
            return Ok(WatchKeyStatus::AlreadyChanged);
        }

        let mut messages = pubsub.on_message();
        tokio::select! {
            msg = messages.next() => {
                match msg {
                    Some(msg) => {
                        let payload: String = msg.get_payload().unwrap_or_default();
                        if payload.is_empty() {
                            return Err("keywatcher: redis GET failed".to_string());
                        }
                        if payload == value {
                            Ok(WatchKeyStatus::NoChange)
                        } else {
                            Ok(WatchKeyStatus::SeenChange)
                        }
                    }
                    None => Ok(WatchKeyStatus::NoChange),
                }
            }
            _ = tokio::time::sleep(timeout) => Ok(WatchKeyStatus::Timeout),
        }
    }
}

impl KeyWatcher {
    pub fn new(client: RedisClient, key: String) -> Self {
        Self {
            client,
            key,
            current_value: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn get_current(&self) -> Option<String> {
        self.current_value.read().await.clone()
    }

    pub async fn poll(&self) -> Result<Option<String>, String> {
        let value = self.client.get(&self.key).await?;
        if value != *self.current_value.read().await {
            let mut guard = self.current_value.write().await;
            *guard = value.clone();
        }
        Ok(value)
    }
}

pub fn notification_channel(key: &str) -> String {
    format!("{NOTIFICATION_CHANNEL_PREFIX}{key}")
}

pub fn resolve_redis_url() -> Option<String> {
    for var in ["GITLAB_RS_REDIS_URL", "REDIS_URL"] {
        if let Ok(url) = std::env::var(var) {
            if !url.is_empty() {
                return Some(url);
            }
        }
    }
    for path in [
        "/var/opt/gitlab/gitlab-rails/etc/resque.yml",
        "/opt/gitlab/embedded/service/gitlab-rails/config/resque.yml",
        "/var/opt/gitlab/gitlab-rails/etc/redis.yml",
    ] {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(url) = parse_resque_url(&content) {
                return Some(url);
            }
        }
    }
    // Leftover omnibus socket is not Rails Redis when redis["enable"]=false.
    if std::path::Path::new("/var/opt/gitlab/redis/redis.socket").exists() {
        return Some("unix:///var/opt/gitlab/redis/redis.socket".to_string());
    }
    Some("redis://127.0.0.1:6379".to_string())
}

pub fn parse_resque_url(content: &str) -> Option<String> {
    let mut in_production = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            in_production = trimmed.starts_with("production:");
            if in_production {
                if let Some(rest) = trimmed.strip_prefix("production:") {
                    let rest = rest.trim().trim_matches('"').trim_matches('\'');
                    if rest.starts_with("redis://") || rest.starts_with("unix://") {
                        return Some(rest.to_string());
                    }
                }
            }
            continue;
        }
        if in_production {
            if let Some(rest) = trimmed.strip_prefix("url:") {
                let url = rest.trim().trim_matches('"').trim_matches('\'');
                if !url.is_empty() {
                    return Some(url.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redis_client_new() {
        let client = RedisClient::new("redis://localhost:6379".to_string());
        assert!(!client.url.is_empty());
    }

    #[tokio::test]
    async fn test_redis_client_not_connected_initially() {
        let client = RedisClient::new("redis://localhost:6379".to_string());
        assert!(!client.is_connected().await);
    }

    #[test]
    fn test_key_watcher_new() {
        let client = RedisClient::new("redis://localhost:6379".to_string());
        let watcher = KeyWatcher::new(client, "test-key".to_string());
        assert_eq!(watcher.key, "test-key");
    }

    #[test]
    fn notification_channel_matches_ce_19_3_1() {
        assert_eq!(
            notification_channel("runner:build_queue:abc"),
            "workhorse:notifications:runner:build_queue:abc"
        );
    }

    #[test]
    fn parse_resque_url_production_block() {
        let yml = "development:\n  url: redis://localhost:6379\nproduction:\n  url: redis://redis/\n";
        assert_eq!(parse_resque_url(yml).as_deref(), Some("redis://redis/"));
    }

    #[test]
    fn parse_resque_url_quoted_and_inline() {
        assert_eq!(
            parse_resque_url("production:\n  url: \"redis://:s3cret@redis:6379/0\"\n")
                .as_deref(),
            Some("redis://:s3cret@redis:6379/0")
        );
        assert_eq!(
            parse_resque_url("production: redis://redis:6379\n").as_deref(),
            Some("redis://redis:6379")
        );
    }
}
