pub mod action;
pub mod counter;
pub mod state;

use action::{CcActionResult, ChallengeManager};
use cdn_common::{CcAction, CcConfig, RedisOps};
use counter::{make_counter_key, match_rule, should_sync_redis};
use once_cell::sync::Lazy;
use prometheus::{register_int_counter_vec, IntCounterVec};
use state::CcState;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

// ── Prometheus metrics ──

static CC_BLOCKED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_cc_blocked_total",
        "CC blocked/challenged requests",
        &["site_id", "action"]
    )
    .unwrap()
});

static CC_REQUESTS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!("cdn_cc_requests_total", "CC checked requests", &["site_id"]).unwrap()
});

/// CC protection engine.
///
/// Check flow:
/// 1. Ban check (moka cache, TTL auto-expire)
/// 2. JS challenge cookie verification (if action=challenge)
/// 3. Longest path prefix rule match
/// 4. Generate counter key (ip / ip_url / ip_path)
/// 5. Hybrid count (local moka + async Redis)
/// 6. Threshold check → block / challenge / delay / log
pub struct CcEngine {
    state: Arc<CcState>,
    challenge: Arc<ChallengeManager>,
    /// Default rate when no rule matches.
    default_rate: u64,
    /// Default window (seconds) when no rule matches.
    default_window: u64,
    /// Default block duration (seconds) when no rule matches.
    default_block_duration: u64,
    /// Optional Redis for distributed counter sync.
    redis: Option<Arc<dyn RedisOps>>,
}

impl CcEngine {
    pub fn new(
        challenge_secret: &str,
        default_rate: u64,
        default_window: u64,
        default_block_duration: u64,
        redis: Option<Arc<dyn RedisOps>>,
    ) -> Self {
        Self {
            state: Arc::new(CcState::new()),
            challenge: Arc::new(ChallengeManager::new(challenge_secret)),
            default_rate,
            default_window,
            default_block_duration,
            redis,
        }
    }

    /// Get a reference to the CC state for external use (admin API).
    pub fn state(&self) -> &CcState {
        &self.state
    }

    /// Run the full CC check for a request.
    pub async fn check(
        &self,
        client_ip: IpAddr,
        uri: &str,
        path: &str,
        cookie_header: Option<&str>,
        cc: &CcConfig,
        site_id: &str,
    ) -> CcActionResult {
        if !cc.enabled {
            return CcActionResult::Allow;
        }

        CC_REQUESTS.with_label_values(&[site_id]).inc();

        // ── Step 1: Ban check ──
        if self.state.is_blocked(site_id, client_ip).await {
            CC_BLOCKED.with_label_values(&[site_id, "block"]).inc();
            return CcActionResult::Block {
                retry_after: self.default_block_duration,
                reason: format!("IP {} is currently banned", client_ip),
            };
        }

        // ── Step 2: Rule matching ──
        let (rate, window, block_duration, action, key_type) =
            if let Some(rule) = match_rule(path, &cc.rules) {
                (
                    rule.rate,
                    rule.window,
                    rule.block_duration,
                    &rule.action,
                    &rule.key_type,
                )
            } else {
                // Use site config values when set (non-zero), fall back to engine defaults
                (
                    if cc.default_rate > 0 {
                        cc.default_rate
                    } else {
                        self.default_rate
                    },
                    if cc.default_window > 0 {
                        cc.default_window
                    } else {
                        self.default_window
                    },
                    if cc.default_block_duration > 0 {
                        cc.default_block_duration
                    } else {
                        self.default_block_duration
                    },
                    &cc.default_action,
                    &cdn_common::CcKeyType::IpUrl,
                )
            };

        // ── Step 3: JS challenge verification (if action=challenge) ──
        if matches!(action, CcAction::Challenge) {
            if let Some(cookie_value) = extract_challenge_cookie(cookie_header) {
                if self.challenge.verify(client_ip, cookie_value) {
                    return CcActionResult::Allow;
                }
            }
        }

        // ── Step 4: Generate counter key ──
        let counter_key = make_counter_key(site_id, client_ip, uri, path, key_type);

        // ── Step 5: Increment counter ──
        let count = self
            .state
            .increment(&counter_key, Duration::from_secs(window))
            .await;

        // Async Redis sync (fire-and-forget)
        // should_sync_redis fires every 10 increments, so delta is always 10
        if should_sync_redis(count) {
            let redis_key = counter::make_redis_key(&counter_key);
            if let Some(ref redis) = self.redis {
                let redis = Arc::clone(redis);
                let expire_secs = window;
                tokio::spawn(async move {
                    if let Err(e) = redis.incr_by_ex(&redis_key, 10, expire_secs).await {
                        log::warn!("[CC] Redis sync failed: key={} err={}", redis_key, e);
                    }
                });
            }
        }

        // ── Step 6: Threshold check ──
        if count <= rate {
            return CcActionResult::Allow;
        }

        // Rate exceeded — apply action
        log::warn!(
            "[CC] rate exceeded: site={} ip={} path={} count={}/{} window={}s",
            site_id,
            client_ip,
            path,
            count,
            rate,
            window
        );

        match action {
            CcAction::Block => {
                // Ban the IP
                self.state
                    .block_ip(site_id, client_ip, Duration::from_secs(block_duration))
                    .await;
                CC_BLOCKED.with_label_values(&[site_id, "block"]).inc();
                CcActionResult::Block {
                    retry_after: block_duration,
                    reason: format!(
                        "rate limit exceeded: {}/{} in {}s on {}",
                        count, rate, window, path
                    ),
                }
            }
            CcAction::Challenge => {
                let cookie_value = self.challenge.issue(client_ip);
                CC_BLOCKED.with_label_values(&[site_id, "challenge"]).inc();
                CcActionResult::Challenge {
                    cookie_value,
                    reason: format!(
                        "JS challenge triggered: {}/{} in {}s on {}",
                        count, rate, window, path
                    ),
                }
            }
            CcAction::Delay => {
                CC_BLOCKED.with_label_values(&[site_id, "delay"]).inc();
                CcActionResult::Delay {
                    delay_ms: 1000, // 1 second delay
                    reason: format!(
                        "rate limit delay: {}/{} in {}s on {}",
                        count, rate, window, path
                    ),
                }
            }
            CcAction::Log => {
                CC_BLOCKED.with_label_values(&[site_id, "log"]).inc();
                CcActionResult::Log {
                    reason: format!(
                        "rate limit logged: {}/{} in {}s on {}",
                        count, rate, window, path
                    ),
                }
            }
        }
    }
}

/// Extract the challenge cookie value from the Cookie header.
fn extract_challenge_cookie(cookie_header: Option<&str>) -> Option<&str> {
    const PREFIX: &str = "__cc_challenge=";
    let header = cookie_header?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(PREFIX) {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdn_common::{CcKeyType, CcRule};

    fn engine() -> CcEngine {
        CcEngine::new("test_secret", 5, 60, 600, None)
    }

    fn cc_enabled(rules: Vec<CcRule>) -> CcConfig {
        CcConfig {
            enabled: true,
            default_rate: 5,
            default_window: 60,
            default_block_duration: 600,
            default_action: CcAction::Block,
            rules,
        }
    }

    #[tokio::test]
    async fn test_disabled_cc_allows() {
        let e = engine();
        let cc = CcConfig::default();
        let result = e
            .check(
                "1.2.3.4".parse().unwrap(),
                "/api",
                "/api",
                None,
                &cc,
                "site1",
            )
            .await;
        assert!(matches!(result, CcActionResult::Allow));
    }

    #[tokio::test]
    async fn test_under_rate_allows() {
        let e = engine();
        let cc = cc_enabled(vec![]);
        let ip: IpAddr = "1.2.3.4".parse().unwrap();

        for _ in 0..5 {
            let result = e.check(ip, "/api", "/api", None, &cc, "site1").await;
            assert!(matches!(result, CcActionResult::Allow));
        }
    }

    #[tokio::test]
    async fn test_over_rate_blocks() {
        let e = engine();
        let cc = cc_enabled(vec![]);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        // Exhaust the rate limit (5 requests)
        for _ in 0..5 {
            let result = e.check(ip, "/test", "/test", None, &cc, "site1").await;
            assert!(matches!(result, CcActionResult::Allow));
        }

        // 6th request should be blocked
        let result = e.check(ip, "/test", "/test", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Block { .. }));
    }

    #[tokio::test]
    async fn test_banned_ip_immediately_blocked() {
        let e = engine();
        let cc = cc_enabled(vec![]);
        let ip: IpAddr = "10.0.0.2".parse().unwrap();

        // Manually ban the IP
        e.state()
            .block_ip("site1", ip, Duration::from_secs(60))
            .await;

        // First request should be blocked immediately
        let result = e.check(ip, "/api", "/api", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Block { .. }));
    }

    #[tokio::test]
    async fn test_rule_matching() {
        let e = engine();
        let cc = cc_enabled(vec![CcRule {
            path: "/api".to_string(),
            rate: 2,
            window: 60,
            block_duration: 300,
            action: CcAction::Block,
            key_type: CcKeyType::IpPath,
        }]);
        let ip: IpAddr = "10.0.0.3".parse().unwrap();

        // 2 requests allowed by the /api rule
        for _ in 0..2 {
            let result = e.check(ip, "/api/v1", "/api/v1", None, &cc, "site1").await;
            assert!(matches!(result, CcActionResult::Allow));
        }

        // 3rd request blocked
        let result = e.check(ip, "/api/v1", "/api/v1", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Block { .. }));
    }

    #[tokio::test]
    async fn test_challenge_action() {
        let e = engine();
        let cc = cc_enabled(vec![CcRule {
            path: "/".to_string(),
            rate: 1,
            window: 60,
            block_duration: 300,
            action: CcAction::Challenge,
            key_type: CcKeyType::Ip,
        }]);
        let ip: IpAddr = "10.0.0.4".parse().unwrap();

        // First request allowed
        let result = e.check(ip, "/", "/", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Allow));

        // Second request triggers challenge
        let result = e.check(ip, "/", "/", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Challenge { .. }));
    }

    #[tokio::test]
    async fn test_challenge_cookie_passes() {
        let e = engine();
        let cc = cc_enabled(vec![CcRule {
            path: "/".to_string(),
            rate: 1,
            window: 60,
            block_duration: 300,
            action: CcAction::Challenge,
            key_type: CcKeyType::Ip,
        }]);
        let ip: IpAddr = "10.0.0.5".parse().unwrap();

        // Issue a valid challenge cookie
        let cookie_value = e.challenge.issue(ip);
        let cookie_header = format!("{}={}", ChallengeManager::cookie_name(), cookie_value);

        // Even over rate, valid challenge cookie should allow
        let _ = e.check(ip, "/", "/", None, &cc, "site1").await; // count=1
        let result = e
            .check(ip, "/", "/", Some(&cookie_header), &cc, "site1")
            .await;
        assert!(matches!(result, CcActionResult::Allow));
    }

    #[tokio::test]
    async fn test_log_action() {
        let e = engine();
        let cc = cc_enabled(vec![CcRule {
            path: "/".to_string(),
            rate: 1,
            window: 60,
            block_duration: 300,
            action: CcAction::Log,
            key_type: CcKeyType::Ip,
        }]);
        let ip: IpAddr = "10.0.0.6".parse().unwrap();

        let _ = e.check(ip, "/", "/", None, &cc, "site1").await;
        let result = e.check(ip, "/", "/", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Log { .. }));
    }

    #[tokio::test]
    async fn test_different_sites_independent() {
        let e = engine();
        let cc = cc_enabled(vec![]);
        let ip: IpAddr = "10.0.0.7".parse().unwrap();

        // Exhaust rate on site1
        for _ in 0..5 {
            e.check(ip, "/", "/", None, &cc, "site1").await;
        }
        let result = e.check(ip, "/", "/", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Block { .. }));

        // site2 should still be allowed
        let result = e.check(ip, "/", "/", None, &cc, "site2").await;
        assert!(matches!(result, CcActionResult::Allow));
    }

    #[test]
    fn test_extract_challenge_cookie() {
        let name = ChallengeManager::cookie_name();
        let header = format!("session=abc; {}=token123; other=xyz", name);
        assert_eq!(extract_challenge_cookie(Some(&header)), Some("token123"));
    }

    #[test]
    fn test_extract_challenge_cookie_missing() {
        assert_eq!(extract_challenge_cookie(Some("session=abc")), None);
        assert_eq!(extract_challenge_cookie(None), None);
    }

    #[test]
    fn test_extract_challenge_cookie_no_substring_match() {
        let name = ChallengeManager::cookie_name();
        // A cookie whose name is a prefix of the challenge cookie name must NOT match
        let header = format!("{}_fake=malicious; other=xyz", name);
        assert_eq!(extract_challenge_cookie(Some(&header)), None);
    }
}
