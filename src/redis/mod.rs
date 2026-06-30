#![allow(dead_code, unused_imports)]
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

#[derive(Clone)]
pub struct RedisClient {
    manager: Arc<RwLock<Option<ConnectionManager>>>,
    url: String,
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
}
