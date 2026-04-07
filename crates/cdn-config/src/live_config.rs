use cdn_common::SiteConfig;
use std::collections::HashMap;
use std::sync::Arc;

/// Dynamic site configuration, atomically swapped via ArcSwap.
#[derive(Debug, Clone, Default)]
pub struct LiveConfig {
    pub sites: HashMap<String, Arc<SiteConfig>>,
    pub domain_index: HashMap<String, String>,
    pub wildcard_index: HashMap<String, String>,
}

impl LiveConfig {
    /// Build domain indexes from the sites map.
    pub fn build_indexes(&mut self) {
        self.domain_index.clear();
        self.wildcard_index.clear();

        for (site_id, site) in &self.sites {
            if !site.enabled {
                continue;
            }
            for domain in &site.domains {
                let domain_lower = domain.to_lowercase();
                if domain_lower.starts_with("*.") {
                    self.wildcard_index
                        .insert(domain_lower, site_id.clone());
                } else {
                    self.domain_index
                        .insert(domain_lower, site_id.clone());
                }
            }
        }
    }

    /// Match a host to a site config.
    /// Priority: exact match → single-level wildcard match → None.
    pub fn match_site(&self, host: &str) -> Option<Arc<SiteConfig>> {
        let host = normalize_host(host);

        // 1. Exact match
        if let Some(site_id) = self.domain_index.get(&host) {
            if let Some(site) = self.sites.get(site_id) {
                if site.enabled {
                    return Some(Arc::clone(site));
                }
            }
        }

        // 2. Wildcard match: foo.example.com → *.example.com
        if let Some(parent) = host.split_once('.').map(|(_, p)| p) {
            let wildcard_key = format!("*.{}", parent);
            if let Some(site_id) = self.wildcard_index.get(&wildcard_key) {
                if let Some(site) = self.sites.get(site_id) {
                    if site.enabled {
                        return Some(Arc::clone(site));
                    }
                }
            }
        }

        None
    }

    /// Get site count and domain count.
    pub fn stats(&self) -> (usize, usize) {
        (
            self.sites.len(),
            self.domain_index.len() + self.wildcard_index.len(),
        )
    }
}

/// Normalize host: strip port, lowercase.
fn normalize_host(host: &str) -> String {
    let host = if let Some(pos) = host.rfind(':') {
        // Only strip if the part after ':' is all digits (port)
        if host[pos + 1..].chars().all(|c| c.is_ascii_digit()) {
            &host[..pos]
        } else {
            host
        }
    } else {
        host
    };
    host.to_lowercase()
}

/// Filter sites by node labels.
/// A site matches if its target_labels is empty (matches all nodes)
/// or if at least one of its target_labels is in the node's labels.
pub fn filter_sites_by_labels(
    sites: HashMap<String, Arc<SiteConfig>>,
    node_labels: &std::collections::HashSet<String>,
) -> HashMap<String, Arc<SiteConfig>> {
    sites
        .into_iter()
        .filter(|(_, site)| {
            if site.target_labels.is_empty() {
                return true; // No labels = matches all nodes
            }
            site.target_labels.iter().any(|l| node_labels.contains(l))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdn_common::*;

    fn make_site(id: &str, domains: Vec<&str>) -> SiteConfig {
        SiteConfig {
            site_id: id.to_string(),
            enabled: true,
            domains: domains.into_iter().map(|s| s.to_string()).collect(),
            target_labels: Vec::new(),
            origins: vec![OriginConfig {
                id: "origin-1".to_string(),
                host: "127.0.0.1".to_string(),
                port: 80,
                weight: 10,
                protocol: OriginProtocol::Http,
                backup: false,
                enabled: true,
                sni: None,
                verify_ssl: false,
                target_labels: Vec::new(),
            }],
            load_balancer: LoadBalancerConfig::default(),
            protocol: ProtocolConfig::default(),
            ssl: SslSiteConfig::default(),
            cache: CacheConfig::default(),
            waf: WafConfig::default(),
            cc: CcConfig::default(),
            headers: HeadersConfig::default(),
            domain_redirect: None,
            timeouts: TimeoutsConfig::default(),
        }
    }

    #[test]
    fn test_exact_match() {
        let mut config = LiveConfig::default();
        config
            .sites
            .insert("100".to_string(), Arc::new(make_site("100", vec!["example.com", "www.example.com"])));
        config.build_indexes();

        assert!(config.match_site("example.com").is_some());
        assert!(config.match_site("www.example.com").is_some());
        assert!(config.match_site("other.com").is_none());
    }

    #[test]
    fn test_wildcard_match() {
        let mut config = LiveConfig::default();
        config
            .sites
            .insert("200".to_string(), Arc::new(make_site("200", vec!["*.example.com"])));
        config.build_indexes();

        assert!(config.match_site("foo.example.com").is_some());
        assert!(config.match_site("bar.example.com").is_some());
        // Multi-level subdomain should NOT match
        assert!(config.match_site("a.b.example.com").is_none());
        // Bare domain should NOT match wildcard
        assert!(config.match_site("example.com").is_none());
    }

    #[test]
    fn test_port_stripping() {
        let mut config = LiveConfig::default();
        config
            .sites
            .insert("100".to_string(), Arc::new(make_site("100", vec!["example.com"])));
        config.build_indexes();

        assert!(config.match_site("example.com:443").is_some());
        assert!(config.match_site("example.com:8080").is_some());
    }

    #[test]
    fn test_case_insensitive() {
        let mut config = LiveConfig::default();
        config
            .sites
            .insert("100".to_string(), Arc::new(make_site("100", vec!["Example.COM"])));
        config.build_indexes();

        assert!(config.match_site("example.com").is_some());
        assert!(config.match_site("EXAMPLE.COM").is_some());
    }

    #[test]
    fn test_disabled_site() {
        let mut site = make_site("100", vec!["example.com"]);
        site.enabled = false;
        let mut config = LiveConfig::default();
        config.sites.insert("100".to_string(), Arc::new(site));
        config.build_indexes();

        assert!(config.match_site("example.com").is_none());
    }
}
