#![allow(dead_code)]

use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct CacheState {
    pub entries: Arc<RwLock<HashMap<String, CacheEntry>>>,
    pub max_size: usize,
    pub default_ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub data: Bytes,
    pub content_type: String,
    pub created_at: Instant,
    pub ttl: Duration,
    pub hits: u64,
}

impl CacheState {
    pub fn new(max_size: usize, default_ttl: Duration) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            max_size,
            default_ttl,
        }
    }

    pub async fn get(&self, key: &str) -> Option<CacheEntry> {
        let mut entries = self.entries.write().await;

        if let Some(entry) = entries.get_mut(key) {
            // Check if entry has expired
            if entry.created_at.elapsed() >= entry.ttl {
                entries.remove(key);
                return None;
            }

            // Increment hit count
            entry.hits += 1;

            Some(entry.clone())
        } else {
            None
        }
    }

    pub async fn set(&self, key: String, data: Bytes, content_type: String, ttl: Option<Duration>) {
        let mut entries = self.entries.write().await;

        // Check if cache is full
        if entries.len() >= self.max_size {
            // Remove oldest entry
            if let Some(oldest_key) = entries.keys().next().cloned() {
                entries.remove(&oldest_key);
            }
        }

        let entry = CacheEntry {
            data,
            content_type,
            created_at: Instant::now(),
            ttl: ttl.unwrap_or(self.default_ttl),
            hits: 0,
        };

        entries.insert(key, entry);
    }

    pub async fn remove(&self, key: &str) -> bool {
        let mut entries = self.entries.write().await;
        entries.remove(key).is_some()
    }

    pub async fn remove_by_prefix(&self, prefix: &str) -> usize {
        let mut entries = self.entries.write().await;
        let keys: Vec<String> = entries.keys()
            .filter(|k| k.contains(prefix))
            .cloned()
            .collect();
        let count = keys.len();
        for key in keys {
            entries.remove(&key);
        }
        count
    }

    pub async fn clear(&self) {
        let mut entries = self.entries.write().await;
        entries.clear();
    }

    pub async fn size(&self) -> usize {
        let entries = self.entries.read().await;
        entries.len()
    }

    pub async fn stats(&self) -> CacheStats {
        let entries = self.entries.read().await;
        let total_hits: u64 = entries.values().map(|e| e.hits).sum();
        let total_size: usize = entries.values().map(|e| e.data.len()).sum();

        CacheStats {
            entries: entries.len(),
            total_hits,
            total_size,
            max_size: self.max_size,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub entries: usize,
    pub total_hits: u64,
    pub total_size: usize,
    pub max_size: usize,
}

impl CacheStats {
    pub fn hit_rate(&self) -> f64 {
        if self.entries == 0 {
            0.0
        } else {
            self.total_hits as f64 / self.entries as f64
        }
    }
}
