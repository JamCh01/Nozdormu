use crate::context::ProtocolType;
use crate::dns::DnsResolver;
use crate::health::HealthChecker;
use cdn_common::{
    AdaptiveWeightConfig, LbAlgorithm, OriginConfig, OriginProtocol, SiteConfig,
};
use dashmap::DashMap;
use pingora::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Per-origin sliding window stats for adaptive weight adjustment.
struct OriginStats {
    /// Ring buffer of (latency_ms, is_error) samples.
    samples: VecDeque<(f64, bool)>,
    window_size: usize,
    last_update: Instant,
}

impl OriginStats {
    fn new(window_size: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(window_size),
            window_size,
            last_update: Instant::now(),
        }
    }
}

/// Summary of origin stats for admin API.
pub struct OriginStatsSummary {
    pub p99_latency: Option<f64>,
    pub error_rate: f64,
    pub sample_count: usize,
}

/// Dynamic load balancer that selects origins based on health, algorithm, and protocol.
pub struct DynamicBalancer {
    pub health: Arc<HealthChecker>,
    pub dns: Arc<DnsResolver>,
    rr_counter: AtomicUsize,
    /// Active connection counts per (site_id, origin_id).
    /// Key: "site_id\0origin_id" (same format as HealthChecker).
    active_conns: DashMap<String, AtomicU32>,
    /// Per-origin performance stats for adaptive weight adjustment.
    /// Key: "site_id\0origin_id".
    origin_stats: DashMap<String, OriginStats>,
}

impl DynamicBalancer {
    pub fn new(health: Arc<HealthChecker>, dns: Arc<DnsResolver>) -> Self {
        Self {
            health,
            dns,
            rr_counter: AtomicUsize::new(0),
            active_conns: DashMap::new(),
            origin_stats: DashMap::new(),
        }
    }

    /// Increment active connection count for an origin.
    pub fn conn_inc(&self, site_id: &str, origin_id: &str) {
        let key = format!("{}\0{}", site_id, origin_id);
        self.active_conns
            .entry(key)
            .or_insert_with(|| AtomicU32::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement active connection count for an origin.
    pub fn conn_dec(&self, site_id: &str, origin_id: &str) {
        let key = format!("{}\0{}", site_id, origin_id);
        if let Some(counter) = self.active_conns.get(&key) {
            // Saturating: avoid underflow from double-dec edge cases
            counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                if v > 0 { Some(v - 1) } else { None }
            }).ok();
        }
    }

    /// Get active connection count for an origin.
    fn active_conn_count(&self, site_id: &str, origin_id: &str) -> u32 {
        let key = format!("{}\0{}", site_id, origin_id);
        self.active_conns
            .get(&key)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Record a completed response for adaptive weight tracking.
    /// Called from `logging()` callback for every request that reached an origin.
    pub fn record_response(
        &self,
        site_id: &str,
        origin_id: &str,
        latency_ms: f64,
        is_error: bool,
        window_size: usize,
    ) {
        let key = format!("{}\0{}", site_id, origin_id);
        let mut entry = self
            .origin_stats
            .entry(key)
            .or_insert_with(|| OriginStats::new(window_size));
        let stats = entry.value_mut();
        if stats.samples.len() >= stats.window_size {
            stats.samples.pop_front();
        }
        stats.samples.push_back((latency_ms, is_error));
        stats.last_update = Instant::now();
    }

    /// Compute effective weight for an origin based on adaptive stats.
    /// Returns static weight when adaptive is disabled or insufficient data.
    pub fn effective_weight(
        &self,
        site_id: &str,
        origin: &OriginConfig,
        config: &AdaptiveWeightConfig,
    ) -> u32 {
        if !config.enabled {
            return origin.weight;
        }
        let key = format!("{}\0{}", site_id, origin.id);
        let multiplier = match self.origin_stats.get(&key) {
            Some(entry) => {
                let stats = entry.value();
                // Stale data (>60s no traffic) or empty → no penalty
                if stats.last_update.elapsed().as_secs() > 60
                    || stats.samples.is_empty()
                {
                    return origin.weight;
                }
                compute_multiplier(stats, config)
            }
            None => return origin.weight,
        };
        std::cmp::max(1, (origin.weight as f64 * multiplier) as u32)
    }

    /// Get stats summary for admin API.
    pub fn get_origin_stats_summary(
        &self,
        site_id: &str,
        origin_id: &str,
    ) -> OriginStatsSummary {
        let key = format!("{}\0{}", site_id, origin_id);
        match self.origin_stats.get(&key) {
            Some(entry) => {
                let stats = entry.value();
                let sample_count = stats.samples.len();
                let p99 = if sample_count >= 10 {
                    Some(percentile_latency(&stats.samples, 0.99))
                } else {
                    None
                };
                let error_count =
                    stats.samples.iter().filter(|(_, e)| *e).count();
                let error_rate = if sample_count > 0 {
                    error_count as f64 / sample_count as f64
                } else {
                    0.0
                };
                OriginStatsSummary {
                    p99_latency: p99,
                    error_rate,
                    sample_count,
                }
            }
            None => OriginStatsSummary {
                p99_latency: None,
                error_rate: 0.0,
                sample_count: 0,
            },
        }
    }

    /// Select an origin and build an HttpPeer.
    ///
    /// Flow:
    /// 1. Filter healthy + enabled primary origins
    /// 2. If none → fallback to healthy backup origins
    /// 3. If none → error
    /// 4. Apply LB algorithm to select one
    /// 5. DNS resolve
    /// 6. Build HttpPeer with TLS/SNI/protocol settings
    pub async fn select_peer(
        &self,
        site: &SiteConfig,
        client_ip: Option<IpAddr>,
        protocol_type: &ProtocolType,
    ) -> Result<(Box<HttpPeer>, OriginConfig)> {
        // Step 1: Filter healthy primary origins
        let primaries: Vec<&OriginConfig> = site
            .origins
            .iter()
            .filter(|o| o.enabled && !o.backup)
            .filter(|o| self.health.is_healthy(&site.site_id, &o.id))
            .collect();

        // Step 2: Fallback to backup origins
        let candidates = if primaries.is_empty() {
            let backups: Vec<&OriginConfig> = site
                .origins
                .iter()
                .filter(|o| o.enabled && o.backup)
                .filter(|o| self.health.is_healthy(&site.site_id, &o.id))
                .collect();
            if backups.is_empty() {
                log::error!("[Balancer] no healthy origins for site={}", site.site_id);
                return Err(pingora::Error::new(ErrorType::ConnectProxyFailure));
            }
            log::warn!("[Balancer] using backup origins for site={}", site.site_id);
            backups
        } else {
            primaries
        };

        // Step 3: Compute effective weights (adaptive adjustment)
        let adaptive_cfg = &site.load_balancer.adaptive_weight;
        let eff_weights: Vec<u32> = candidates
            .iter()
            .map(|o| self.effective_weight(&site.site_id, o, adaptive_cfg))
            .collect();

        // Step 4: Apply LB algorithm
        let selected = match &site.load_balancer.algorithm {
            LbAlgorithm::RoundRobin => {
                self.select_round_robin(&candidates, &eff_weights)
            }
            LbAlgorithm::IpHash => {
                let ip = client_ip
                    .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
                self.select_ip_hash(&candidates, &eff_weights, ip)
            }
            LbAlgorithm::Random => {
                self.select_weighted_random(&candidates, &eff_weights)
            }
            LbAlgorithm::LeastConn => {
                self.select_least_conn(
                    &candidates,
                    &eff_weights,
                    &site.site_id,
                )
            }
        };

        // Step 4: DNS resolve
        let addr = self
            .dns
            .resolve_to_socket(&selected.host, selected.port)
            .await
            .ok_or_else(|| {
                log::error!(
                    "[Balancer] DNS resolution failed: {}:{}",
                    selected.host,
                    selected.port
                );
                pingora::Error::new(ErrorType::ConnectProxyFailure)
            })?;

        // Step 5: Build HttpPeer
        let use_tls = selected.protocol == OriginProtocol::Https;
        let sni = selected
            .sni
            .clone()
            .unwrap_or_else(|| selected.host.clone());

        let mut peer = if use_tls {
            HttpPeer::new(addr, true, sni)
        } else {
            HttpPeer::new(addr, false, String::new())
        };

        // Protocol-specific peer options
        match protocol_type {
            ProtocolType::Grpc(_) => {
                peer.options.set_http_version(2, 2);
                peer.options.max_h2_streams = 10;
            }
            ProtocolType::WebSocket | ProtocolType::Sse => {
                peer.options.read_timeout = None;
            }
            ProtocolType::Http => {}
        }

        Ok((Box::new(peer), selected.clone()))
    }

    /// Weighted round-robin selection using effective weights.
    fn select_round_robin<'a>(
        &self,
        candidates: &[&'a OriginConfig],
        eff_weights: &[u32],
    ) -> &'a OriginConfig {
        let total_weight: u32 = eff_weights.iter().sum();
        if total_weight == 0 {
            let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed)
                % candidates.len();
            return candidates[idx];
        }

        let counter = self.rr_counter.fetch_add(1, Ordering::Relaxed);
        let target = (counter % total_weight as usize) as u32;
        let mut cumulative = 0u32;
        for (i, origin) in candidates.iter().enumerate() {
            cumulative += eff_weights[i];
            if target < cumulative {
                return origin;
            }
        }
        candidates.last().unwrap()
    }

    /// Consistent hash (Ketama ring) selection for IP-based sticky routing.
    /// Each origin gets `eff_weight * VNODES_PER_WEIGHT` virtual nodes on the ring.
    /// Adding/removing an origin only remaps ~1/N of requests.
    fn select_ip_hash<'a>(
        &self,
        candidates: &[&'a OriginConfig],
        eff_weights: &[u32],
        ip: IpAddr,
    ) -> &'a OriginConfig {
        let ring = build_hash_ring_weighted(candidates, eff_weights);
        if ring.is_empty() {
            // All candidates have weight 0; fall back to simple index
            let hash = sip_hash(&ip.to_string());
            return candidates[hash as usize % candidates.len()];
        }
        let ip_hash = sip_hash(&ip.to_string());
        let idx =
            match ring.binary_search_by_key(&ip_hash, |&(point, _)| point) {
                Ok(i) => i,
                Err(i) => {
                    if i >= ring.len() {
                        0
                    } else {
                        i
                    }
                }
            };
        candidates[ring[idx].1]
    }

    /// Weighted random selection using effective weights.
    fn select_weighted_random<'a>(
        &self,
        candidates: &[&'a OriginConfig],
        eff_weights: &[u32],
    ) -> &'a OriginConfig {
        let total_weight: u32 = eff_weights.iter().sum();
        if total_weight == 0 {
            let idx = fastrand::usize(..candidates.len());
            return candidates[idx];
        }
        let target = fastrand::u32(..total_weight);
        let mut cumulative = 0u32;
        for (i, origin) in candidates.iter().enumerate() {
            cumulative += eff_weights[i];
            if target < cumulative {
                return origin;
            }
        }
        candidates.last().unwrap()
    }

    /// Least-connections selection: pick the origin with the fewest active connections.
    /// Ties are broken by effective weight (higher wins), then by position (first wins).
    fn select_least_conn<'a>(
        &self,
        candidates: &[&'a OriginConfig],
        eff_weights: &[u32],
        site_id: &str,
    ) -> &'a OriginConfig {
        let mut best_idx = 0;
        let mut best_conns =
            self.active_conn_count(site_id, &candidates[0].id);

        for i in 1..candidates.len() {
            let conns =
                self.active_conn_count(site_id, &candidates[i].id);
            if conns < best_conns
                || (conns == best_conns
                    && eff_weights[i] > eff_weights[best_idx])
            {
                best_idx = i;
                best_conns = conns;
            }
        }
        candidates[best_idx]
    }
}

/// Hash a string to u64 using SipHash (std DefaultHasher).
fn sip_hash(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Virtual nodes per unit of weight for the consistent hash ring.
/// With default weight=10, each origin gets 400 vnodes.
const VNODES_PER_WEIGHT: u32 = 40;

/// Build a Ketama-style consistent hash ring from candidates using effective weights.
/// Returns sorted Vec of (ring_point, candidate_index).
fn build_hash_ring_weighted(
    candidates: &[&OriginConfig],
    eff_weights: &[u32],
) -> Vec<(u64, usize)> {
    let mut ring: Vec<(u64, usize)> = Vec::new();
    for (idx, origin) in candidates.iter().enumerate() {
        let vnodes = eff_weights[idx] * VNODES_PER_WEIGHT;
        for i in 0..vnodes {
            let key = format!("{}-{}", origin.id, i);
            ring.push((sip_hash(&key), idx));
        }
    }
    ring.sort_unstable_by_key(|&(point, _)| point);
    ring
}

/// Build a hash ring using static origin weights (for tests).
#[cfg(test)]
fn build_hash_ring(candidates: &[&OriginConfig]) -> Vec<(u64, usize)> {
    let weights: Vec<u32> = candidates.iter().map(|o| o.weight).collect();
    build_hash_ring_weighted(candidates, &weights)
}

/// Compute weight multiplier from origin stats. Returns value in [0.1, 1.0].
fn compute_multiplier(
    stats: &OriginStats,
    config: &AdaptiveWeightConfig,
) -> f64 {
    // P99 latency factor
    let latency_factor = if stats.samples.len() < 10 {
        1.0 // Not enough data
    } else {
        let p99 = percentile_latency(&stats.samples, 0.99);
        penalty_factor(
            p99,
            config.latency_threshold_ms,
            config.latency_threshold_ms * 4.0,
        )
    };

    // Error rate factor
    let error_count = stats.samples.iter().filter(|(_, e)| *e).count();
    let error_rate = error_count as f64 / stats.samples.len() as f64;
    let error_factor = penalty_factor(
        error_rate,
        config.error_threshold,
        config.error_threshold * 4.0,
    );

    latency_factor * error_factor
}

/// Linear penalty: value <= low → 1.0, value >= high → 0.1, else interpolate.
fn penalty_factor(value: f64, low: f64, high: f64) -> f64 {
    if value <= low {
        return 1.0;
    }
    if value >= high {
        return 0.1;
    }
    1.0 - 0.9 * (value - low) / (high - low)
}

/// Compute percentile from samples (sort-based).
fn percentile_latency(
    samples: &VecDeque<(f64, bool)>,
    pct: f64,
) -> f64 {
    let mut latencies: Vec<f64> =
        samples.iter().map(|(l, _)| *l).collect();
    latencies.sort_unstable_by(|a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    let idx =
        ((latencies.len() as f64 * pct).ceil() as usize).saturating_sub(1);
    latencies[idx.min(latencies.len() - 1)]
}

// fastrand is extracted to crate::utils::rand
use crate::utils::rand as fastrand;

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(id: &str, weight: u32, backup: bool) -> OriginConfig {
        OriginConfig {
            id: id.to_string(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            weight,
            protocol: OriginProtocol::Http,
            backup,
            enabled: true,
            sni: None,
            verify_ssl: false,
            target_labels: vec![],
        }
    }

    /// Helper: build effective weights from static weights (no adaptive).
    fn static_weights(candidates: &[&OriginConfig]) -> Vec<u32> {
        candidates.iter().map(|o| o.weight).collect()
    }

    fn default_adaptive() -> AdaptiveWeightConfig {
        AdaptiveWeightConfig::default()
    }

    fn enabled_adaptive() -> AdaptiveWeightConfig {
        AdaptiveWeightConfig {
            enabled: true,
            ..AdaptiveWeightConfig::default()
        }
    }

    #[test]
    fn test_sip_hash_deterministic() {
        assert_eq!(sip_hash("1.2.3.4"), sip_hash("1.2.3.4"));
        assert_ne!(sip_hash("1.2.3.4"), sip_hash("5.6.7.8"));
    }

    #[test]
    fn test_hash_ring_build() {
        let o1 = origin("a", 10, false);
        let o2 = origin("b", 10, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let ring = build_hash_ring(&candidates);

        // Should have 10*40 + 10*40 = 800 entries
        assert_eq!(ring.len(), 800);
        // Ring should be sorted
        for window in ring.windows(2) {
            assert!(window[0].0 <= window[1].0);
        }
        // Both origins should be present
        assert!(ring.iter().any(|&(_, idx)| idx == 0));
        assert!(ring.iter().any(|&(_, idx)| idx == 1));
    }

    #[test]
    fn test_hash_ring_zero_weight() {
        let o1 = origin("a", 0, false);
        let o2 = origin("b", 0, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let ring = build_hash_ring(&candidates);
        assert!(ring.is_empty());
    }

    #[test]
    fn test_round_robin_cycles() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("a", 1, false);
        let o2 = origin("b", 1, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let w = static_weights(&candidates);

        let first = balancer.select_round_robin(&candidates, &w).id.clone();
        let second =
            balancer.select_round_robin(&candidates, &w).id.clone();
        // Should alternate
        assert_ne!(first, second);
    }

    #[test]
    fn test_ip_hash_consistent() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("a", 10, false);
        let o2 = origin("b", 10, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let w = static_weights(&candidates);
        let ip: IpAddr = "1.2.3.4".parse().unwrap();

        let first =
            balancer.select_ip_hash(&candidates, &w, ip).id.clone();
        let second =
            balancer.select_ip_hash(&candidates, &w, ip).id.clone();
        // Same IP should always select the same origin
        assert_eq!(first, second);
    }

    #[test]
    fn test_weighted_random_respects_weights() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        // One origin with weight 100, one with weight 0
        let o1 = origin("heavy", 100, false);
        let o2 = origin("zero", 0, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let w = static_weights(&candidates);

        // With total_weight=100, all selections should go to "heavy"
        for _ in 0..10 {
            let selected =
                balancer.select_weighted_random(&candidates, &w);
            assert_eq!(selected.id, "heavy");
        }
    }

    #[test]
    fn test_least_conn_picks_fewest() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("a", 10, false);
        let o2 = origin("b", 10, false);
        let o3 = origin("c", 10, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2, &o3];
        let w = static_weights(&candidates);

        // Simulate: a=3, b=1, c=2 active connections
        for _ in 0..3 {
            balancer.conn_inc("site1", "a");
        }
        balancer.conn_inc("site1", "b");
        for _ in 0..2 {
            balancer.conn_inc("site1", "c");
        }

        let selected =
            balancer.select_least_conn(&candidates, &w, "site1");
        assert_eq!(selected.id, "b");
    }

    #[test]
    fn test_least_conn_tie_breaks_by_weight() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("light", 5, false);
        let o2 = origin("heavy", 20, false);
        // Both have 0 active connections — tie broken by weight
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let w = static_weights(&candidates);

        let selected =
            balancer.select_least_conn(&candidates, &w, "site1");
        assert_eq!(selected.id, "heavy");
    }

    #[test]
    fn test_conn_inc_dec_saturating() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        assert_eq!(balancer.active_conn_count("s", "o"), 0);
        balancer.conn_inc("s", "o");
        balancer.conn_inc("s", "o");
        assert_eq!(balancer.active_conn_count("s", "o"), 2);
        balancer.conn_dec("s", "o");
        assert_eq!(balancer.active_conn_count("s", "o"), 1);
        // Double-dec past zero should not underflow
        balancer.conn_dec("s", "o");
        balancer.conn_dec("s", "o");
        assert_eq!(balancer.active_conn_count("s", "o"), 0);
    }

    #[test]
    fn test_least_conn_zero_conns_all() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        // All equal weight, all zero conns — should pick first
        let o1 = origin("a", 10, false);
        let o2 = origin("b", 10, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let w = static_weights(&candidates);

        let selected =
            balancer.select_least_conn(&candidates, &w, "site1");
        assert_eq!(selected.id, "a");
    }

    #[test]
    fn test_ip_hash_minimal_remap() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("a", 10, false);
        let o2 = origin("b", 10, false);
        let o3 = origin("c", 10, false);

        let three: Vec<&OriginConfig> = vec![&o1, &o2, &o3];
        let w3 = static_weights(&three);
        let two: Vec<&OriginConfig> = vec![&o1, &o2]; // origin "c" removed
        let w2 = static_weights(&two);

        let mut remapped = 0;
        let total = 1000;
        for i in 0..total {
            let ip: IpAddr =
                format!("10.0.{}.{}", i / 256, i % 256).parse().unwrap();
            let with_three =
                balancer.select_ip_hash(&three, &w3, ip).id.clone();
            let with_two =
                balancer.select_ip_hash(&two, &w2, ip).id.clone();
            // IPs that were on "c" must remap; IPs on "a" or "b" should mostly stay
            if with_three != "c" && with_three != with_two {
                remapped += 1;
            }
        }
        assert!(
            remapped < total / 10,
            "Too many remaps: {remapped}/{total} — consistent hashing property violated"
        );
    }

    #[test]
    fn test_ip_hash_weight_distribution() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("heavy", 30, false);
        let o2 = origin("light", 10, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];
        let w = static_weights(&candidates);

        let mut heavy_count = 0;
        let total = 2000;
        for i in 0..total {
            let ip: IpAddr = format!(
                "10.{}.{}.{}",
                i / 65536,
                (i / 256) % 256,
                i % 256
            )
            .parse()
            .unwrap();
            if balancer.select_ip_hash(&candidates, &w, ip).id == "heavy" {
                heavy_count += 1;
            }
        }
        // Expected: 75% to heavy (30/(30+10)). Accept 60%-90%.
        let ratio = heavy_count as f64 / total as f64;
        assert!(
            (0.60..=0.90).contains(&ratio),
            "Weight distribution off: heavy got {:.1}% (expected ~75%)",
            ratio * 100.0
        );
    }

    // ── Adaptive weight tests ──

    #[test]
    fn test_penalty_factor() {
        // Below threshold → 1.0
        assert_eq!(penalty_factor(0.0, 500.0, 2000.0), 1.0);
        assert_eq!(penalty_factor(500.0, 500.0, 2000.0), 1.0);
        // Above max → 0.1
        assert_eq!(penalty_factor(2000.0, 500.0, 2000.0), 0.1);
        assert_eq!(penalty_factor(3000.0, 500.0, 2000.0), 0.1);
        // Midpoint → ~0.55
        let mid = penalty_factor(1250.0, 500.0, 2000.0);
        assert!((mid - 0.55).abs() < 0.01, "mid={mid}");
    }

    #[test]
    fn test_percentile_latency() {
        let samples: VecDeque<(f64, bool)> =
            (1..=100).map(|i| (i as f64, false)).collect();
        let p99 = percentile_latency(&samples, 0.99);
        assert_eq!(p99, 99.0);
        let p50 = percentile_latency(&samples, 0.50);
        assert_eq!(p50, 50.0);
    }

    #[test]
    fn test_effective_weight_no_data() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);
        let o = origin("a", 10, false);
        let cfg = enabled_adaptive();
        // No stats recorded → returns static weight
        assert_eq!(balancer.effective_weight("s", &o, &cfg), 10);
    }

    #[test]
    fn test_effective_weight_healthy_origin() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);
        let o = origin("a", 10, false);
        let cfg = enabled_adaptive();
        // Record 50 fast, successful responses
        for _ in 0..50 {
            balancer.record_response("s", "a", 50.0, false, 100);
        }
        assert_eq!(balancer.effective_weight("s", &o, &cfg), 10);
    }

    #[test]
    fn test_effective_weight_slow_origin() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);
        let o = origin("a", 10, false);
        let cfg = enabled_adaptive();
        // Record 100 very slow responses (2000ms = 4x threshold)
        for _ in 0..100 {
            balancer.record_response("s", "a", 2000.0, false, 100);
        }
        let eff = balancer.effective_weight("s", &o, &cfg);
        assert_eq!(eff, 1, "Slow origin should be penalized to floor");
    }

    #[test]
    fn test_effective_weight_error_origin() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);
        let o = origin("a", 10, false);
        let cfg = enabled_adaptive();
        // Record 100 responses: 40% errors (>= 4x threshold of 0.1)
        for i in 0..100 {
            balancer.record_response("s", "a", 50.0, i < 40, 100);
        }
        let eff = balancer.effective_weight("s", &o, &cfg);
        assert_eq!(eff, 1, "High error rate should penalize to floor");
    }

    #[test]
    fn test_effective_weight_stale_data() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);
        let o = origin("a", 10, false);
        let cfg = enabled_adaptive();
        // Record bad data then make it stale
        for _ in 0..100 {
            balancer.record_response("s", "a", 5000.0, true, 100);
        }
        // Manually set last_update to >60s ago
        if let Some(mut entry) = balancer.origin_stats.get_mut("s\0a") {
            entry.last_update =
                Instant::now() - std::time::Duration::from_secs(120);
        }
        // Stale data → returns static weight
        assert_eq!(balancer.effective_weight("s", &o, &cfg), 10);
    }

    #[test]
    fn test_record_response_window_rollover() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);
        // Record 150 samples with window_size=100
        for i in 0..150 {
            balancer.record_response("s", "a", i as f64, false, 100);
        }
        let entry = balancer.origin_stats.get("s\0a").unwrap();
        assert_eq!(entry.samples.len(), 100);
        // Oldest should be 50.0 (first 50 were evicted)
        assert_eq!(entry.samples[0].0, 50.0);
    }

    #[test]
    fn test_adaptive_disabled() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);
        let o = origin("a", 10, false);
        let cfg = default_adaptive(); // disabled
        // Record terrible stats
        for _ in 0..100 {
            balancer.record_response("s", "a", 5000.0, true, 100);
        }
        // Disabled → always returns static weight
        assert_eq!(balancer.effective_weight("s", &o, &cfg), 10);
    }

    #[test]
    fn test_round_robin_uses_effective_weight() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("healthy", 10, false);
        let o2 = origin("degraded", 10, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];

        // Simulate: healthy=10, degraded=1 (effective)
        let eff_weights = vec![10u32, 1u32];

        let mut healthy_count = 0;
        let total = 1100;
        for _ in 0..total {
            let selected =
                balancer.select_round_robin(&candidates, &eff_weights);
            if selected.id == "healthy" {
                healthy_count += 1;
            }
        }
        // Expected: ~90.9% to healthy (10/11). Accept 85%-96%.
        let ratio = healthy_count as f64 / total as f64;
        assert!(
            (0.85..=0.96).contains(&ratio),
            "Healthy got {:.1}% (expected ~90.9%)",
            ratio * 100.0
        );
    }

    #[test]
    fn test_get_origin_stats_summary() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        // No data
        let summary = balancer.get_origin_stats_summary("s", "a");
        assert_eq!(summary.sample_count, 0);
        assert!(summary.p99_latency.is_none());
        assert_eq!(summary.error_rate, 0.0);

        // Record some data
        for i in 0..50 {
            balancer.record_response(
                "s",
                "a",
                (i + 1) as f64 * 10.0,
                i >= 45, // last 5 are errors
                100,
            );
        }
        let summary = balancer.get_origin_stats_summary("s", "a");
        assert_eq!(summary.sample_count, 50);
        assert!(summary.p99_latency.is_some());
        let err = summary.error_rate;
        assert!(
            (err - 0.1).abs() < 0.01,
            "error_rate={err}, expected 0.1"
        );
    }
}
