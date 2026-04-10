use dashmap::DashMap;
use std::sync::Arc;
use std::time::SystemTime;

/// Per-origin health state, protected by DashMap's per-shard lock.
#[derive(Debug, Clone)]
struct OriginHealth {
    healthy: bool,
    consecutive_failures: u32,
    consecutive_successes: u32,
    /// Timestamp of last active health check probe.
    last_active_check: Option<SystemTime>,
    /// Result of last active health check probe.
    last_active_success: Option<bool>,
}

impl Default for OriginHealth {
    fn default() -> Self {
        Self {
            healthy: true,
            consecutive_failures: 0,
            consecutive_successes: 0,
            last_active_check: None,
            last_active_success: None,
        }
    }
}

/// Detailed health info returned by `get_detail()` for admin API.
#[derive(Debug, Clone)]
pub struct HealthDetail {
    pub healthy: bool,
    pub consecutive_successes: u32,
    pub consecutive_failures: u32,
    pub last_active_check: Option<SystemTime>,
    pub last_active_success: Option<bool>,
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
                site_id,
                origin_id
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
                site_id,
                origin_id,
                h.consecutive_failures
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

    /// Record active health check probe result metadata.
    pub fn record_active_check(&self, site_id: &str, origin_id: &str, success: bool) {
        let key = make_key(site_id, origin_id);
        let mut entry = self.state.entry(key).or_default();
        let h = entry.value_mut();
        h.last_active_check = Some(SystemTime::now());
        h.last_active_success = Some(success);
    }

    /// Get detailed health info for admin API.
    pub fn get_detail(&self, site_id: &str, origin_id: &str) -> HealthDetail {
        let key = make_key(site_id, origin_id);
        self.state
            .get(&key)
            .map(|h| HealthDetail {
                healthy: h.healthy,
                consecutive_successes: h.consecutive_successes,
                consecutive_failures: h.consecutive_failures,
                last_active_check: h.last_active_check,
                last_active_success: h.last_active_success,
            })
            .unwrap_or(HealthDetail {
                healthy: true,
                consecutive_successes: 0,
                consecutive_failures: 0,
                last_active_check: None,
                last_active_success: None,
            })
    }

    /// Remove an origin's health state (cleanup when probe task removed).
    pub fn remove_origin(&self, site_id: &str, origin_id: &str) {
        let key = make_key(site_id, origin_id);
        self.state.remove(&key);
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

    #[test]
    fn test_get_detail_unknown_origin() {
        let hc = HealthChecker::new(3, 2);
        let detail = hc.get_detail("site1", "origin1");
        assert!(detail.healthy);
        assert_eq!(detail.consecutive_successes, 0);
        assert_eq!(detail.consecutive_failures, 0);
        assert!(detail.last_active_check.is_none());
        assert!(detail.last_active_success.is_none());
    }

    #[test]
    fn test_get_detail_after_failures() {
        let hc = HealthChecker::new(3, 2);
        hc.record_failure("site1", "origin1");
        hc.record_failure("site1", "origin1");
        let detail = hc.get_detail("site1", "origin1");
        assert!(detail.healthy); // only 2 failures, threshold is 3
        assert_eq!(detail.consecutive_failures, 2);
        assert_eq!(detail.consecutive_successes, 0);
    }

    #[test]
    fn test_record_active_check() {
        let hc = HealthChecker::new(3, 2);
        hc.record_active_check("site1", "origin1", true);
        let detail = hc.get_detail("site1", "origin1");
        assert!(detail.last_active_check.is_some());
        assert_eq!(detail.last_active_success, Some(true));

        hc.record_active_check("site1", "origin1", false);
        let detail = hc.get_detail("site1", "origin1");
        assert_eq!(detail.last_active_success, Some(false));
    }

    #[test]
    fn test_remove_origin() {
        let hc = HealthChecker::new(3, 2);
        for _ in 0..3 {
            hc.record_failure("site1", "origin1");
        }
        assert!(!hc.is_healthy("site1", "origin1"));

        hc.remove_origin("site1", "origin1");
        // After removal, unknown origin defaults to healthy
        assert!(hc.is_healthy("site1", "origin1"));
    }
}
