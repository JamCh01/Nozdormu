use async_trait::async_trait;

/// Minimal async Redis interface for cross-crate use.
///
/// This trait allows cdn-middleware and cdn-cache to use Redis
/// without depending on cdn-proxy (avoiding circular dependencies).
/// cdn-proxy's RedisPool implements this trait.
#[async_trait]
pub trait RedisOps: Send + Sync {
    /// GET a string value. Returns Ok(None) for missing keys, Err for connection failures.
    async fn get(&self, key: &str) -> Result<Option<String>, String>;

    /// SET a string value with expiry (seconds).
    async fn setex(&self, key: &str, seconds: u64, value: &str) -> Result<(), String>;

    /// DELETE a key.
    async fn del(&self, key: &str) -> Result<(), String>;

    /// Increment a key by delta, returning the new value.
    async fn incr_by(&self, key: &str, delta: u64) -> Result<u64, String>;

    /// Increment a key by delta with expiry (seconds), returning the new value.
    /// Atomically sets the TTL so counters don't leak in Redis.
    async fn incr_by_ex(&self, key: &str, delta: u64, expire_secs: u64) -> Result<u64, String>;
}
