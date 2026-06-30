#![allow(dead_code)]

use bytes::BytesMut;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct MemoryPool {
    inner: Arc<Mutex<MemoryPoolInner>>,
}

#[derive(Debug)]
struct MemoryPoolInner {
    buffer_pool: VecDeque<BytesMut>,
    max_pool_size: usize,
    buffer_capacity: usize,
    allocated: usize,
    reused: usize,
}

impl MemoryPool {
    pub fn new(max_pool_size: usize, buffer_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryPoolInner {
                buffer_pool: VecDeque::with_capacity(max_pool_size),
                max_pool_size,
                buffer_capacity,
                allocated: 0,
                reused: 0,
            })),
        }
    }

    pub async fn acquire(&self) -> BytesMut {
        let mut inner = self.inner.lock().await;
        if let Some(mut buffer) = inner.buffer_pool.pop_front() {
            buffer.clear();
            inner.reused += 1;
            buffer
        } else {
            inner.allocated += 1;
            BytesMut::with_capacity(inner.buffer_capacity)
        }
    }

    pub async fn release(&self, buffer: BytesMut) {
        let mut inner = self.inner.lock().await;
        if inner.buffer_pool.len() < inner.max_pool_size {
            inner.buffer_pool.push_back(buffer);
        }
    }

    pub async fn stats(&self) -> MemoryPoolStats {
        let inner = self.inner.lock().await;
        MemoryPoolStats {
            pool_size: inner.buffer_pool.len(),
            max_pool_size: inner.max_pool_size,
            allocated: inner.allocated,
            reused: inner.reused,
            reuse_rate: if inner.allocated + inner.reused > 0 {
                inner.reused as f64 / (inner.allocated + inner.reused) as f64
            } else {
                0.0
            },
        }
    }

    pub async fn clear(&self) {
        let mut inner = self.inner.lock().await;
        inner.buffer_pool.clear();
    }
}

#[derive(Debug, Clone)]
pub struct MemoryPoolStats {
    pub pool_size: usize,
    pub max_pool_size: usize,
    pub allocated: usize,
    pub reused: usize,
    pub reuse_rate: f64,
}

impl Default for MemoryPool {
    fn default() -> Self {
        Self::new(100, 8192)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_pool_acquire_release() {
        let pool = MemoryPool::new(10, 1024);

        let buffer = pool.acquire().await;
        assert_eq!(buffer.capacity(), 1024);

        pool.release(buffer).await;

        let stats = pool.stats().await;
        assert_eq!(stats.pool_size, 1);
        assert_eq!(stats.allocated, 1);
    }

    #[tokio::test]
    async fn test_memory_pool_reuse() {
        let pool = MemoryPool::new(10, 1024);

        let buffer1 = pool.acquire().await;
        pool.release(buffer1).await;

        let buffer2 = pool.acquire().await;
        pool.release(buffer2).await;

        let stats = pool.stats().await;
        assert_eq!(stats.reused, 1);
        assert_eq!(stats.allocated, 1);
    }

    #[tokio::test]
    async fn test_memory_pool_max_size() {
        let pool = MemoryPool::new(2, 1024);

        let buffer1 = pool.acquire().await;
        let buffer2 = pool.acquire().await;
        let buffer3 = pool.acquire().await;

        pool.release(buffer1).await;
        pool.release(buffer2).await;
        pool.release(buffer3).await;

        let stats = pool.stats().await;
        assert_eq!(stats.pool_size, 2);
    }
}
