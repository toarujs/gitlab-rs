#![allow(dead_code)]
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct GitalyConnectionCache {
    connections: Arc<RwLock<HashMap<String, GitalyConnectionInfo>>>,
}

#[derive(Debug, Clone)]
pub struct GitalyConnectionInfo {
    pub address: String,
    pub token: String,
    pub connected: bool,
    pub last_used: std::time::Instant,
}

#[derive(Debug, Clone)]
pub struct GitalyServer {
    pub address: String,
    pub token: String,
    pub call_metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct SmartHTTPHandler {
    pub server: GitalyServer,
    pub repository_storage: String,
    pub relative_path: String,
}

impl GitalyConnectionCache {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get_or_create(&self, address: &str, token: &str) -> GitalyConnectionInfo {
        let mut guard = self.connections.write().await;

        if let Some(info) = guard.get(address) {
            let mut updated = info.clone();
            updated.last_used = std::time::Instant::now();
            guard.insert(address.to_string(), updated.clone());
            return updated;
        }

        let info = GitalyConnectionInfo {
            address: address.to_string(),
            token: token.to_string(),
            connected: true,
            last_used: std::time::Instant::now(),
        };

        guard.insert(address.to_string(), info.clone());
        info
    }

    pub async fn remove(&self, address: &str) {
        let mut guard = self.connections.write().await;
        guard.remove(address);
    }

    pub async fn len(&self) -> usize {
        self.connections.read().await.len()
    }
}

impl Default for GitalyConnectionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartHTTPHandler {
    pub fn new(server: GitalyServer, repository_storage: String, relative_path: String) -> Self {
        Self {
            server,
            repository_storage,
            relative_path,
        }
    }

    pub fn service_name(&self) -> String {
        format!(
            "gitaly://{}/{}",
            self.server.address, self.relative_path
        )
    }
}

pub fn parse_gitaly_address(address: &str) -> Option<(String, u16)> {
    let parts: Vec<&str> = address.rsplitn(2, ':').collect();
    if parts.len() == 2 {
        let host = parts[1].to_string();
        if let Ok(port) = parts[0].parse::<u16>() {
            return Some((host, port));
        }
    }

    let host = address.to_string();
    Some((host, 8075))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gitaly_address() {
        let result = parse_gitaly_address("localhost:8075");
        assert!(result.is_some());
        let (host, port) = result.unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 8075);
    }

    #[test]
    fn test_parse_gitaly_address_default_port() {
        let result = parse_gitaly_address("gitaly.internal");
        assert!(result.is_some());
        let (host, port) = result.unwrap();
        assert_eq!(host, "gitaly.internal");
        assert_eq!(port, 8075);
    }

    #[tokio::test]
    async fn test_connection_cache() {
        let cache = GitalyConnectionCache::new();
        assert_eq!(cache.len().await, 0);

        let info = cache.get_or_create("localhost:8075", "token123").await;
        assert!(info.connected);
        assert_eq!(cache.len().await, 1);

        let info2 = cache.get_or_create("localhost:8075", "token456").await;
        assert!(info.connected);
        assert!(info2.connected);
        assert_eq!(cache.len().await, 1);
    }

    #[test]
    fn test_smart_http_handler() {
        let server = GitalyServer {
            address: "localhost:8075".to_string(),
            token: "secret".to_string(),
            call_metadata: HashMap::new(),
        };

        let handler = SmartHTTPHandler::new(
            server,
            "default".to_string(),
            "@hashed/aa/bb.git".to_string(),
        );

        assert!(handler.service_name().contains("gitaly://"));
        assert!(handler.service_name().contains("localhost:8075"));
    }
}
