use dashmap::DashMap;
use std::sync::Arc;

/// Per-origin health state, protected by DashMap's per-shard lock.
#[derive(Debug, Clone)]
struct OriginHealth {
    healthy: bool,
    consecutive_failures: u32,
    consecutive_successes: u32,
}

impl Default for OriginHealth {
    fn default() -> Self {
        Self {
            healthy: true,
            consecutive_failures: 0,
            consecutive_successes: 0,
        }
    }
}

/// Health checker for origin servers.
///
/// Tracks health status per (site_id, origin_id) pair.
/// - Passive: record_failure/record_success called from logging() callback
/// - Active: TODO Phase 7 integration — BackgroundService HTTP probe
///
/// Thresholds:
/// - 3 consecutive failures → mark unhealthy
/// - 2 consecutive successes → mark healthy
pub struct HealthChecker {
    /// All health state in a single map to avoid cross-map race conditions.
    /// Key format: "site_id\0origin_id" (single String to halve allocations).
    state: Arc<DashMap<String, OriginHealth>>,
    /// Failures needed to mark unhealthy
    unhealthy_threshold: u32,
    /// Successes needed to mark healthy
    healthy_threshold: u32,
}

/// Build a DashMap key from site_id and origin_id using NUL separator.
fn make_key(site_id: &str, origin_id: &str) -> String {
    let mut key = String::with_capacity(site_id.len() + 1 + origin_id.len());
    key.push_str(site_id);
    key.push('\0');
    key.push_str(origin_id);
    key
}

impl HealthChecker {
    pub fn new(unhealthy_threshold: u32, healthy_threshold: u32) -> Self {
        Self {
            state: Arc::new(DashMap::new()),
            unhealthy_threshold,
            healthy_threshold,
        }
    }

    /// Check if an origin is healthy. Unknown origins are assumed healthy.
    pub fn is_healthy(&self, site_id: &str, origin_id: &str) -> bool {
        let key = make_key(site_id, origin_id);
        self.state.get(&key).map(|v| v.healthy).unwrap_or(true)
    }

    /// Record a successful response from an origin (passive health check).
    pub fn record_success(&self, site_id: &str, origin_id: &str) {
        let key = make_key(site_id, origin_id);
        let mut entry = self.state.entry(key).or_default();
        let h = entry.value_mut();

        h.consecutive_failures = 0;
        h.consecutive_successes += 1;

        if h.consecutive_successes >= self.healthy_threshold && !h.healthy {
            log::info!(
                "[Health] origin recovered: site={} origin={}",
                site_id, origin_id
            );
            h.healthy = true;
        }
    }

    /// Record a failed response from an origin (passive health check).
    pub fn record_failure(&self, site_id: &str, origin_id: &str) {
        let key = make_key(site_id, origin_id);
        let mut entry = self.state.entry(key).or_default();
        let h = entry.value_mut();

        h.consecutive_successes = 0;
        h.consecutive_failures += 1;

        if h.consecutive_failures >= self.unhealthy_threshold {
            h.healthy = false;
            log::warn!(
                "[Health] origin marked unhealthy: site={} origin={} failures={}",
                site_id, origin_id, h.consecutive_failures
            );
        }
    }

    /// Manually set health status (for admin API).
    pub fn set_status(&self, site_id: &str, origin_id: &str, healthy: bool) {
        let key = make_key(site_id, origin_id);
        let mut entry = self.state.entry(key).or_default();
        let h = entry.value_mut();
        h.healthy = healthy;
        h.consecutive_failures = 0;
        h.consecutive_successes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_origin_is_healthy() {
        let hc = HealthChecker::new(3, 2);
        assert!(hc.is_healthy("site1", "origin1"));
    }

    #[test]
    fn test_failures_mark_unhealthy() {
        let hc = HealthChecker::new(3, 2);
        hc.record_failure("site1", "origin1");
        assert!(hc.is_healthy("site1", "origin1")); // 1 failure, still healthy
        hc.record_failure("site1", "origin1");
        assert!(hc.is_healthy("site1", "origin1")); // 2 failures, still healthy
        hc.record_failure("site1", "origin1");
        assert!(!hc.is_healthy("site1", "origin1")); // 3 failures → unhealthy
    }

    #[test]
    fn test_successes_recover() {
        let hc = HealthChecker::new(3, 2);
        // Mark unhealthy
        for _ in 0..3 {
            hc.record_failure("site1", "origin1");
        }
        assert!(!hc.is_healthy("site1", "origin1"));

        // Recover
        hc.record_success("site1", "origin1");
        assert!(!hc.is_healthy("site1", "origin1")); // 1 success, still unhealthy
        hc.record_success("site1", "origin1");
        assert!(hc.is_healthy("site1", "origin1")); // 2 successes → healthy
    }

    #[test]
    fn test_failure_resets_success_counter() {
        let hc = HealthChecker::new(3, 2);
        for _ in 0..3 {
            hc.record_failure("site1", "origin1");
        }
        assert!(!hc.is_healthy("site1", "origin1"));

        hc.record_success("site1", "origin1"); // 1 success
        hc.record_failure("site1", "origin1"); // resets success counter
        hc.record_success("site1", "origin1"); // 1 success again (not 2)
        assert!(!hc.is_healthy("site1", "origin1")); // still unhealthy
    }

    #[test]
    fn test_independent_origins() {
        let hc = HealthChecker::new(3, 2);
        for _ in 0..3 {
            hc.record_failure("site1", "origin1");
        }
        assert!(!hc.is_healthy("site1", "origin1"));
        assert!(hc.is_healthy("site1", "origin2")); // different origin
        assert!(hc.is_healthy("site2", "origin1")); // different site
    }

    #[test]
    fn test_manual_set_status() {
        let hc = HealthChecker::new(3, 2);
        hc.set_status("site1", "origin1", false);
        assert!(!hc.is_healthy("site1", "origin1"));
        hc.set_status("site1", "origin1", true);
        assert!(hc.is_healthy("site1", "origin1"));
    }
}
