#![allow(dead_code, unused_imports)]
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum QueueError {
    TooManyRequests,
    QueueingTimedout,
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::TooManyRequests => write!(f, "too many requests queued"),
            QueueError::QueueingTimedout => write!(f, "queueing timed out"),
        }
    }
}

impl std::error::Error for QueueError {}

#[derive(Clone)]
pub struct Queue {
    name: String,
    busy_semaphore: Arc<Semaphore>,
    queue_semaphore: Arc<Semaphore>,
    timeout: Duration,
    limit: usize,
    queue_limit: usize,
}

pub struct QueueGuard {
    _busy_permit: tokio::sync::OwnedSemaphorePermit,
    _queue_permit: tokio::sync::OwnedSemaphorePermit,
}

impl Queue {
    pub fn new(name: String, limit: usize, queue_limit: usize, timeout: Duration) -> Self {
        Self {
            name,
            busy_semaphore: Arc::new(Semaphore::new(limit)),
            queue_semaphore: Arc::new(Semaphore::new(limit + queue_limit)),
            timeout,
            limit,
            queue_limit,
        }
    }

    pub async fn acquire(&self) -> Result<QueueGuard, QueueError> {
        let queue_permit = match self.queue_semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return Err(QueueError::TooManyRequests),
        };

        let busy_permit = match tokio::time::timeout(
            self.timeout,
            self.busy_semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(p)) => p,
            Ok(Err(_)) => return Err(QueueError::QueueingTimedout),
            Err(_) => return Err(QueueError::QueueingTimedout),
        };

        Ok(QueueGuard {
            _busy_permit: busy_permit,
            _queue_permit: queue_permit,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn queue_limit(&self) -> usize {
        self.queue_limit
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_queue_acquire_release() {
        let queue = Queue::new("test".to_string(), 2, 5, Duration::from_secs(1));
        let guard = queue.acquire().await.unwrap();
        assert_eq!(queue.limit(), 2);
        drop(guard);
    }

    #[tokio::test]
    async fn test_queue_too_many_requests() {
        let queue = Queue::new("test".to_string(), 1, 0, Duration::from_secs(1));
        let _guard1 = queue.acquire().await.unwrap();
        let result = queue.acquire().await;
        assert!(matches!(result, Err(QueueError::TooManyRequests)));
    }

    #[tokio::test]
    async fn test_queue_timeout() {
        let queue = Queue::new("test".to_string(), 1, 1, Duration::from_millis(50));
        let _guard1 = queue.acquire().await.unwrap();
        let result = queue.acquire().await;
        assert!(matches!(result, Err(QueueError::QueueingTimedout)));
    }
}
