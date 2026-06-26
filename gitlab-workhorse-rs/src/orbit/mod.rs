#![allow(dead_code, unused_imports)]
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct GkgServer {
    pub address: String,
    pub token: String,
    connections: Arc<RwLock<HashMap<String, bool>>>,
}

impl GkgServer {
    pub fn new(address: String, token: String) -> Self {
        Self {
            address,
            token,
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn is_connected(&self) -> bool {
        !self.connections.read().await.is_empty()
    }

    pub async fn register_connection(&self, id: String) {
        self.connections.write().await.insert(id, true);
    }

    pub async fn remove_connection(&self, id: &str) {
        self.connections.write().await.remove(id);
    }

    pub fn service_address(&self) -> String {
        format!("gitaly://{}", self.address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gkg_server_new() {
        let server = GkgServer::new("localhost:50051".to_string(), "token".to_string());
        assert!(!server.is_connected().await);
        assert_eq!(server.service_address(), "gitaly://localhost:50051");
    }

    #[tokio::test]
    async fn test_gkg_server_connections() {
        let server = GkgServer::new("localhost:50051".to_string(), "token".to_string());
        server.register_connection("conn1".to_string()).await;
        assert!(server.is_connected().await);

        server.remove_connection("conn1").await;
        assert!(!server.is_connected().await);
    }
}
