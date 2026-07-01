#![allow(dead_code)]

use bytes::Bytes;
use moka::future::Cache;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CacheState {
    pub cache: Cache<String, CacheEntry>,
    pub max_entry_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub data: Bytes,
    pub content_type: String,
    pub hits: Arc<AtomicU64>,
}

impl CacheState {
    pub fn new(max_size: usize, max_entry_bytes: usize, default_ttl: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(max_size as u64)
                .time_to_live(default_ttl)
                .build(),
            max_entry_bytes,
        }
    }

    pub async fn get(&self, key: &str) -> Option<CacheEntry> {
        self.cache.get(key).await.map(|entry| {
            entry.hits.fetch_add(1, Ordering::Relaxed);
            entry
        })
    }

    pub async fn set(&self, key: String, data: Bytes, content_type: String, _ttl: Option<Duration>) {
        if data.len() > self.max_entry_bytes {
            return;
        }
        let entry = CacheEntry {
            data,
            content_type,
            hits: Arc::new(AtomicU64::new(0)),
        };
        self.cache.insert(key, entry).await;
    }

    pub async fn remove(&self, key: &str) -> bool {
        let existed = self.cache.get(key).await.is_some();
        self.cache.invalidate(key).await;
        existed
    }

    pub async fn remove_by_prefix(&self, prefix: &str) -> usize {
        let prefix = prefix.to_string();
        let keys_to_remove: Vec<String> = self.cache.iter()
            .filter(|entry| entry.0.contains(&prefix))
            .map(|entry| entry.0.as_ref().clone())
            .collect();
        let count = keys_to_remove.len();
        for key in keys_to_remove {
            self.cache.invalidate(&key).await;
        }
        count
    }

    pub async fn clear(&self) {
        self.cache.invalidate_all();
    }

    pub async fn size(&self) -> usize {
        self.cache.entry_count() as usize
    }

    pub async fn stats(&self) -> CacheStats {
        let entries = self.cache.entry_count() as usize;
        let total_hits: u64 = self.cache.iter()
            .map(|e| e.1.hits.load(Ordering::Relaxed))
            .sum();
        let total_size: usize = self.cache.iter()
            .map(|e| e.1.data.len())
            .sum();

        CacheStats {
            entries,
            total_hits,
            total_size,
            max_size: 1000,
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
