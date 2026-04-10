use crate::context::ProtocolType;
use crate::dns::DnsResolver;
use crate::health::HealthChecker;
use cdn_common::{LbAlgorithm, OriginConfig, OriginProtocol, SiteConfig};
use pingora::prelude::*;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Dynamic load balancer that selects origins based on health, algorithm, and protocol.
pub struct DynamicBalancer {
    pub health: Arc<HealthChecker>,
    pub dns: Arc<DnsResolver>,
    rr_counter: AtomicUsize,
}

impl DynamicBalancer {
    pub fn new(health: Arc<HealthChecker>, dns: Arc<DnsResolver>) -> Self {
        Self {
            health,
            dns,
            rr_counter: AtomicUsize::new(0),
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

        // Step 3: Apply LB algorithm
        let selected = match &site.load_balancer.algorithm {
            LbAlgorithm::RoundRobin => self.select_round_robin(&candidates),
            LbAlgorithm::IpHash => {
                let ip = client_ip.unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
                self.select_ip_hash(&candidates, ip)
            }
            LbAlgorithm::Random => self.select_weighted_random(&candidates),
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

    /// Weighted round-robin selection.
    fn select_round_robin<'a>(&self, candidates: &[&'a OriginConfig]) -> &'a OriginConfig {
        let total_weight: u32 = candidates.iter().map(|o| o.weight).sum();
        if total_weight == 0 {
            let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % candidates.len();
            return candidates[idx];
        }

        let counter = self.rr_counter.fetch_add(1, Ordering::Relaxed);
        let target = (counter % total_weight as usize) as u32;
        let mut cumulative = 0u32;
        for origin in candidates {
            cumulative += origin.weight;
            if target < cumulative {
                return origin;
            }
        }
        candidates.last().unwrap()
    }

    /// IP hash selection using DJB2 hash.
    fn select_ip_hash<'a>(&self, candidates: &[&'a OriginConfig], ip: IpAddr) -> &'a OriginConfig {
        let hash = djb2_hash(&ip.to_string());
        let total_weight: u32 = candidates.iter().map(|o| o.weight).sum();
        if total_weight == 0 {
            return candidates[hash as usize % candidates.len()];
        }
        let target = hash % (total_weight as u64);
        let mut cumulative = 0u64;
        for origin in candidates {
            cumulative += origin.weight as u64;
            if target < cumulative {
                return origin;
            }
        }
        candidates.last().unwrap()
    }

    /// Weighted random selection.
    fn select_weighted_random<'a>(&self, candidates: &[&'a OriginConfig]) -> &'a OriginConfig {
        let total_weight: u32 = candidates.iter().map(|o| o.weight).sum();
        if total_weight == 0 {
            let idx = fastrand::usize(..candidates.len());
            return candidates[idx];
        }
        let target = fastrand::u32(..total_weight);
        let mut cumulative = 0u32;
        for origin in candidates {
            cumulative += origin.weight;
            if target < cumulative {
                return origin;
            }
        }
        candidates.last().unwrap()
    }
}

/// DJB2 hash function for IP hash load balancing.
fn djb2_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
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

    #[test]
    fn test_djb2_deterministic() {
        assert_eq!(djb2_hash("1.2.3.4"), djb2_hash("1.2.3.4"));
        assert_ne!(djb2_hash("1.2.3.4"), djb2_hash("5.6.7.8"));
    }

    #[test]
    fn test_round_robin_cycles() {
        let hc = Arc::new(HealthChecker::new(3, 2));
        let dns = Arc::new(DnsResolver::new());
        let balancer = DynamicBalancer::new(hc, dns);

        let o1 = origin("a", 1, false);
        let o2 = origin("b", 1, false);
        let candidates: Vec<&OriginConfig> = vec![&o1, &o2];

        let first = balancer.select_round_robin(&candidates).id.clone();
        let second = balancer.select_round_robin(&candidates).id.clone();
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
        let ip: IpAddr = "1.2.3.4".parse().unwrap();

        let first = balancer.select_ip_hash(&candidates, ip).id.clone();
        let second = balancer.select_ip_hash(&candidates, ip).id.clone();
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

        // With total_weight=100, all selections should go to "heavy"
        for _ in 0..10 {
            let selected = balancer.select_weighted_random(&candidates);
            assert_eq!(selected.id, "heavy");
        }
    }
}
