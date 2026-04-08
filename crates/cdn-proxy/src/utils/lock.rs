use crate::utils::redis_pool::RedisPool;
use std::time::Duration;

/// Distributed lock using Redis SETNX + Lua script for atomic release.
/// Key format: nozdormu:lock:{name}
pub struct DistributedLock {
    pub key: String,
    pub owner: String,
    pub ttl_secs: u64,
}

impl DistributedLock {
    /// Create a new lock configuration.
    pub fn new(name: &str, node_id: &str, ttl_secs: u64) -> Self {
        Self {
            key: format!("nozdormu:lock:{}", name),
            owner: format!("{}:{}", node_id, std::process::id()),
            ttl_secs,
        }
    }

    /// Attempt to acquire the lock. Returns true if acquired.
    pub async fn acquire(&self, pool: &RedisPool) -> bool {
        pool.set_nx_ex(&self.key, &self.owner, self.ttl_secs).await
    }

    /// Release the lock. Returns true if this owner held it.
    pub async fn release(&self, pool: &RedisPool) -> bool {
        match pool
            .eval_script(Self::release_script(), &[&self.key], &[&self.owner])
            .await
        {
            Ok(redis::Value::Int(1)) => true,
            _ => false,
        }
    }

    /// Renew the lock TTL. Returns true if renewed.
    pub async fn renew(&self, pool: &RedisPool) -> bool {
        let ttl_str = self.ttl_secs.to_string();
        match pool
            .eval_script(Self::renew_script(), &[&self.key], &[&self.owner, &ttl_str])
            .await
        {
            Ok(redis::Value::Int(1)) => true,
            _ => false,
        }
    }

    /// Retry acquiring the lock with delay between attempts.
    pub async fn acquire_with_retry(
        &self,
        pool: &RedisPool,
        max_attempts: u32,
        retry_delay: Duration,
    ) -> bool {
        for attempt in 0..max_attempts {
            if self.acquire(pool).await {
                return true;
            }
            if attempt + 1 < max_attempts {
                tokio::time::sleep(retry_delay).await;
            }
        }
        false
    }

    /// Redis command to acquire the lock (for reference/testing).
    pub fn acquire_cmd(&self) -> Vec<String> {
        vec![
            "SET".to_string(),
            self.key.clone(),
            self.owner.clone(),
            "NX".to_string(),
            "EX".to_string(),
            self.ttl_secs.to_string(),
        ]
    }

    /// Lua script to atomically release the lock.
    pub fn release_script() -> &'static str {
        r#"
        if redis.call("GET", KEYS[1]) == ARGV[1] then
            return redis.call("DEL", KEYS[1])
        else
            return 0
        end
        "#
    }

    /// Lua script to atomically renew the lock TTL.
    pub fn renew_script() -> &'static str {
        r#"
        if redis.call("GET", KEYS[1]) == ARGV[1] then
            return redis.call("EXPIRE", KEYS[1], ARGV[2])
        else
            return 0
        end
        "#
    }
}

/// Well-known lock names for the CDN system.
pub mod lock_names {
    /// Lock for ACME certificate issuance.
    pub fn acme_obtain(domain: &str) -> String {
        format!("acme:obtain:{}", domain)
    }

    /// Lock for renewal scan (only one node scans at a time).
    pub fn renewal_scan() -> String {
        "renewal:scan".to_string()
    }

    /// Lock for renewing a specific domain's certificate.
    pub fn renewal_domain(domain: &str) -> String {
        format!("renewal:{}", domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_key_format() {
        let lock = DistributedLock::new("test:resource", "node-1", 60);
        assert_eq!(lock.key, "nozdormu:lock:test:resource");
        assert!(lock.owner.starts_with("node-1:"));
    }

    #[test]
    fn test_acquire_cmd() {
        let lock = DistributedLock::new("test", "node-1", 30);
        let cmd = lock.acquire_cmd();
        assert_eq!(cmd[0], "SET");
        assert_eq!(cmd[3], "NX");
        assert_eq!(cmd[4], "EX");
        assert_eq!(cmd[5], "30");
    }

    #[test]
    fn test_lock_names() {
        assert_eq!(lock_names::acme_obtain("example.com"), "acme:obtain:example.com");
        assert_eq!(lock_names::renewal_scan(), "renewal:scan");
        assert_eq!(lock_names::renewal_domain("example.com"), "renewal:example.com");
    }
}
