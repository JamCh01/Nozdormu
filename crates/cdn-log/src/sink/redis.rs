use crate::sink::{LogSink, LogSinkError};
use async_trait::async_trait;
use std::sync::Arc;

/// Trait for Redis Stream operations.
///
/// This abstraction avoids pulling `RedisPool` (from cdn-proxy) into cdn-log.
/// The proxy crate implements this trait for its `RedisPool`.
#[async_trait]
pub trait RedisStreamOps: Send + Sync + 'static {
    /// XADD to a Redis Stream with approximate MAXLEN trimming.
    async fn xadd(
        &self,
        stream: &str,
        maxlen: u64,
        field: &str,
        value: &str,
    ) -> Result<(), String>;

    /// Whether the Redis connection is available.
    fn is_available(&self) -> bool;
}

/// Redis Streams log sink.
///
/// The `destination` parameter in `send()` is used as the Redis Stream key.
pub struct RedisStreamSink {
    pool: Arc<dyn RedisStreamOps>,
    max_len: u64,
}

impl RedisStreamSink {
    pub fn new(pool: Arc<dyn RedisStreamOps>, max_len: u64) -> Self {
        Self { pool, max_len }
    }
}

#[async_trait]
impl LogSink for RedisStreamSink {
    async fn send(
        &self,
        destination: &str,
        entries: &[String],
    ) -> Result<(), LogSinkError> {
        if !self.pool.is_available() {
            return Err(LogSinkError::Connection(
                "Redis pool not available".to_string(),
            ));
        }
        for json in entries {
            if let Err(e) = self
                .pool
                .xadd(destination, self.max_len, "data", json)
                .await
            {
                log::warn!("[LogSink:redis] XADD to {} failed: {}", destination, e);
                log::info!("[LogQueue:local] {}", json);
            }
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), LogSinkError> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "redis"
    }
}
