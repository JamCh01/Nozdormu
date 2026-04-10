use moka::future::Cache;
use moka::Expiry;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Value wrapper that carries its own TTL for per-entry expiration.
#[derive(Clone)]
struct TtlValue<V> {
    value: V,
    ttl: Duration,
}

/// Expiry policy that reads TTL from the stored value.
struct TtlExpiry;

impl<K> Expiry<K, TtlValue<()>> for TtlExpiry {
    fn expire_after_create(
        &self,
        _key: &K,
        value: &TtlValue<()>,
        _current_time: Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// Expiry policy for counters — TTL from the stored wrapper.
struct CounterExpiry;

impl Expiry<String, TtlValue<Arc<AtomicU64>>> for CounterExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &TtlValue<Arc<AtomicU64>>,
        _current_time: Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// CC state: ban cache (auto-expire via TTL) + local counters.
/// Equivalent to the original ngx.shared.cc_blocked + ngx.shared.cc_counter.
pub struct CcState {
    /// Blocked IPs: key = "site_id\0ip", per-entry TTL = block_duration.
    blocked: Cache<String, TtlValue<()>>,
    /// Local counters: key = counter_key, per-entry TTL = window duration.
    counters: Cache<String, TtlValue<Arc<AtomicU64>>>,
}

impl Default for CcState {
    fn default() -> Self {
        Self::new()
    }
}

impl CcState {
    pub fn new() -> Self {
        let blocked = Cache::builder()
            .max_capacity(100_000)
            .expire_after(TtlExpiry)
            .build();

        let counters = Cache::builder()
            .max_capacity(500_000)
            .expire_after(CounterExpiry)
            .build();

        Self { blocked, counters }
    }

    /// Block an IP for a site with the given duration.
    pub async fn block_ip(&self, site_id: &str, ip: IpAddr, duration: Duration) {
        let key = block_key(site_id, ip);
        self.blocked
            .insert(
                key,
                TtlValue {
                    value: (),
                    ttl: duration,
                },
            )
            .await;
    }

    /// Unblock an IP for a site.
    pub async fn unblock_ip(&self, site_id: &str, ip: IpAddr) {
        self.blocked.remove(&block_key(site_id, ip)).await;
    }

    /// Check if an IP is currently blocked for a site.
    pub async fn is_blocked(&self, site_id: &str, ip: IpAddr) -> bool {
        self.blocked.contains_key(&block_key(site_id, ip))
    }

    /// Increment a counter and return the new value.
    /// The counter auto-expires after `window` duration.
    pub async fn increment(&self, key: &str, window: Duration) -> u64 {
        let entry = self
            .counters
            .entry_by_ref(key)
            .or_insert_with(async {
                TtlValue {
                    value: Arc::new(AtomicU64::new(0)),
                    ttl: window,
                }
            })
            .await
            .into_value();

        entry.value.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Get the current count for a key (0 if not present).
    pub async fn get_count(&self, key: &str) -> u64 {
        self.counters
            .get(key)
            .await
            .map(|e| e.value.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

fn block_key(site_id: &str, ip: IpAddr) -> String {
    use std::fmt::Write;
    // Pre-allocate: site_id + \0 + max IPv6 len (39) + margin
    let mut key = String::with_capacity(site_id.len() + 1 + 45);
    key.push_str(site_id);
    key.push('\0');
    let _ = write!(key, "{}", ip);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_block_and_check() {
        let state = CcState::new();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();

        assert!(!state.is_blocked("site1", ip).await);
        state.block_ip("site1", ip, Duration::from_secs(60)).await;
        assert!(state.is_blocked("site1", ip).await);

        // Different site should not be blocked
        assert!(!state.is_blocked("site2", ip).await);
    }

    #[tokio::test]
    async fn test_unblock() {
        let state = CcState::new();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();

        state.block_ip("site1", ip, Duration::from_secs(60)).await;
        assert!(state.is_blocked("site1", ip).await);

        state.unblock_ip("site1", ip).await;
        assert!(!state.is_blocked("site1", ip).await);
    }

    #[tokio::test]
    async fn test_counter_increment() {
        let state = CcState::new();
        let window = Duration::from_secs(60);

        assert_eq!(state.get_count("key1").await, 0);
        assert_eq!(state.increment("key1", window).await, 1);
        assert_eq!(state.increment("key1", window).await, 2);
        assert_eq!(state.increment("key1", window).await, 3);
        assert_eq!(state.get_count("key1").await, 3);
    }

    #[tokio::test]
    async fn test_counter_independent_keys() {
        let state = CcState::new();
        let window = Duration::from_secs(60);

        assert_eq!(state.increment("key1", window).await, 1);
        assert_eq!(state.increment("key2", window).await, 1);
        assert_eq!(state.increment("key1", window).await, 2);
        assert_eq!(state.get_count("key1").await, 2);
        assert_eq!(state.get_count("key2").await, 1);
    }
}
