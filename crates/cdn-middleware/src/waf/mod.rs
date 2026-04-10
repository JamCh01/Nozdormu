pub mod body;
pub mod geo;
pub mod ip;

use cdn_common::{WafConfig, WafMode};
use geo::{GeoInfo, GeoIpDb};
use ip::IpCidrSet;
use once_cell::sync::Lazy;
use prometheus::{register_int_counter_vec, IntCounterVec};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

// ── Prometheus metrics ──

static WAF_BLOCKED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_waf_blocked_total",
        "WAF blocked requests",
        &["site_id", "block_type"]
    )
    .unwrap()
});

// ── Compiled WAF IP sets (cached per site) ──

/// Pre-built IP trie sets for a site's WAF rules.
pub struct CompiledWafSets {
    pub whitelist: IpCidrSet,
    pub blacklist: IpCidrSet,
}

impl CompiledWafSets {
    pub fn build(waf: &WafConfig) -> Self {
        Self {
            whitelist: IpCidrSet::new(&waf.rules.ip_whitelist),
            blacklist: IpCidrSet::new(&waf.rules.ip_blacklist),
        }
    }
}

// ── WAF result ──

#[derive(Debug, Clone)]
pub enum WafResult {
    /// Request is allowed to proceed.
    Allow,
    /// Request should be blocked (403).
    Block {
        block_type: &'static str,
        reason: String,
    },
    /// Request is logged but allowed to proceed (log-only mode).
    Log {
        block_type: &'static str,
        reason: String,
    },
}

impl WafResult {
    pub fn is_blocked(&self) -> bool {
        matches!(self, WafResult::Block { .. })
    }
}

// ── WAF engine ──

pub struct WafEngine {
    geo_db: Arc<GeoIpDb>,
}

impl WafEngine {
    /// Create a new WAF engine, loading GeoIP databases from the given directory.
    pub fn new(geoip_dir: &Path) -> Self {
        Self {
            geo_db: Arc::new(GeoIpDb::load_from_dir(geoip_dir)),
        }
    }

    /// Create a WAF engine without GeoIP databases (IP checks only).
    pub fn without_geoip() -> Self {
        Self {
            geo_db: Arc::new(GeoIpDb::empty()),
        }
    }

    /// Get a reference to the GeoIP database for external use (e.g., populating ProxyCtx).
    pub fn geo_db(&self) -> &GeoIpDb {
        &self.geo_db
    }

    /// Run the full WAF check chain for a request.
    ///
    /// Check order (strict):
    /// 1. IP whitelist → hit = allow immediately (skip all subsequent checks)
    /// 2. IP blacklist → hit = block or log
    /// 3. ASN blacklist → hit = block or log (requires GeoIP)
    /// 4. Country whitelist → not in list = deny (fail-closed when country unknown)
    /// 5. Country blacklist → hit = deny
    /// 6. Region/province blacklist → hit = deny (requires City database)
    pub fn check(
        &self,
        client_ip: IpAddr,
        waf: &WafConfig,
        site_id: &str,
    ) -> (WafResult, Option<GeoInfo>) {
        self.check_with_sets(client_ip, waf, site_id, None, None)
    }

    /// Run WAF check with pre-built IP trie sets.
    /// Use this in the hot path to avoid rebuilding tries on every request.
    pub fn check_with_sets(
        &self,
        client_ip: IpAddr,
        waf: &WafConfig,
        site_id: &str,
        whitelist: Option<&IpCidrSet>,
        blacklist: Option<&IpCidrSet>,
    ) -> (WafResult, Option<GeoInfo>) {
        if !waf.enabled {
            return (WafResult::Allow, None);
        }

        let rules = &waf.rules;

        // Build LPM tries only if not provided (avoids per-request rebuild)
        let owned_whitelist;
        let owned_blacklist;
        let whitelist = match whitelist {
            Some(wl) => wl,
            None => {
                owned_whitelist = IpCidrSet::new(&rules.ip_whitelist);
                &owned_whitelist
            }
        };
        let blacklist = match blacklist {
            Some(bl) => bl,
            None => {
                owned_blacklist = IpCidrSet::new(&rules.ip_blacklist);
                &owned_blacklist
            }
        };

        // ── Step 1: IP whitelist → immediate allow ──
        if whitelist.contains(client_ip) {
            return (WafResult::Allow, None);
        }

        // ── Step 2: IP blacklist ──
        if let Some(cidr) = blacklist.longest_match(client_ip) {
            let result = self.make_result(
                &waf.mode,
                "ip_blacklist",
                format!("IP {} matched blacklist {}", client_ip, cidr),
                site_id,
            );
            return (result, None);
        }

        // Steps 3-6 require GeoIP data
        let needs_geo = !rules.asn_blacklist.is_empty()
            || !rules.country_whitelist.is_empty()
            || !rules.country_blacklist.is_empty()
            || !rules.region_blacklist.is_empty()
            || !rules.continent_blacklist.is_empty();

        if !needs_geo {
            return (WafResult::Allow, None);
        }

        let geo = self.geo_db.lookup(client_ip);

        // ── Step 3: ASN blacklist ──
        if let Some(asn) = geo.asn {
            if rules.asn_blacklist.contains(&asn) {
                let result = self.make_result(
                    &waf.mode,
                    "asn_blacklist",
                    format!("ASN {} is blacklisted", asn),
                    site_id,
                );
                return (result, Some(geo));
            }
        }

        // ── Step 4: Country whitelist (fail-closed) ──
        if !rules.country_whitelist.is_empty() {
            match &geo.country_code {
                Some(cc) => {
                    if !rules
                        .country_whitelist
                        .iter()
                        .any(|w| w.eq_ignore_ascii_case(cc))
                    {
                        let result = self.make_result(
                            &waf.mode,
                            "country_whitelist",
                            format!("country {} not in whitelist", cc),
                            site_id,
                        );
                        return (result, Some(geo));
                    }
                }
                None => {
                    // Fail-closed: unknown country is denied when whitelist is active
                    let result = self.make_result(
                        &waf.mode,
                        "country_whitelist",
                        "country unknown, whitelist active — denied".to_string(),
                        site_id,
                    );
                    return (result, Some(geo));
                }
            }
        }

        // ── Step 5: Country blacklist ──
        if let Some(cc) = &geo.country_code {
            if rules
                .country_blacklist
                .iter()
                .any(|b| b.eq_ignore_ascii_case(cc))
            {
                let result = self.make_result(
                    &waf.mode,
                    "country_blacklist",
                    format!("country {} is blacklisted", cc),
                    site_id,
                );
                return (result, Some(geo));
            }
        }

        // ── Step 5.5: Continent blacklist ──
        if let Some(cont) = &geo.continent_code {
            if rules
                .continent_blacklist
                .iter()
                .any(|b| b.eq_ignore_ascii_case(cont))
            {
                let result = self.make_result(
                    &waf.mode,
                    "continent_blacklist",
                    format!("continent {} is blacklisted", cont),
                    site_id,
                );
                return (result, Some(geo));
            }
        }

        // ── Step 6: Region/province blacklist ──
        if let Some(cc) = &geo.country_code {
            let cc_upper = cc.to_uppercase();
            // Case-insensitive lookup: try both the uppercase key and iterate for mismatch
            let blocked_regions = rules.region_blacklist.get(&cc_upper).or_else(|| {
                rules
                    .region_blacklist
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(&cc_upper))
                    .map(|(_, v)| v)
            });
            if let Some(blocked_regions) = blocked_regions {
                if let Some(sub) = &geo.subdivision_code {
                    if blocked_regions.iter().any(|r| r.eq_ignore_ascii_case(sub)) {
                        let result = self.make_result(
                            &waf.mode,
                            "region_blacklist",
                            format!("region {}-{} is blacklisted", cc, sub),
                            site_id,
                        );
                        return (result, Some(geo));
                    }
                }
            }
        }

        (WafResult::Allow, Some(geo))
    }

    /// Create a Block or Log result depending on WAF mode, and increment Prometheus counter.
    fn make_result(
        &self,
        mode: &WafMode,
        block_type: &'static str,
        reason: String,
        site_id: &str,
    ) -> WafResult {
        match mode {
            WafMode::Block => {
                WAF_BLOCKED.with_label_values(&[site_id, block_type]).inc();
                log::warn!(
                    "[WAF] BLOCK site={} type={} reason={}",
                    site_id,
                    block_type,
                    reason
                );
                WafResult::Block { block_type, reason }
            }
            WafMode::Log => {
                WAF_BLOCKED.with_label_values(&[site_id, block_type]).inc();
                log::info!(
                    "[WAF] LOG site={} type={} reason={}",
                    site_id,
                    block_type,
                    reason
                );
                WafResult::Log { block_type, reason }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdn_common::WafRules;
    use ipnet::IpNet;
    use std::collections::HashMap;
    use std::str::FromStr;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    fn net(s: &str) -> IpNet {
        IpNet::from_str(s).unwrap()
    }

    fn engine() -> WafEngine {
        WafEngine::without_geoip()
    }

    fn waf_enabled(rules: WafRules) -> WafConfig {
        WafConfig {
            enabled: true,
            mode: WafMode::Block,
            rules,
            ..Default::default()
        }
    }

    #[test]
    fn test_disabled_waf_allows_all() {
        let e = engine();
        let waf = WafConfig {
            enabled: false,
            ..Default::default()
        };
        let (result, _) = e.check(ip("1.2.3.4"), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));
    }

    #[test]
    fn test_ip_whitelist_bypasses_all() {
        let e = engine();
        let waf = waf_enabled(WafRules {
            ip_whitelist: vec![net("10.0.0.0/8")],
            ip_blacklist: vec![net("10.0.0.0/8")], // also blacklisted — whitelist wins
            ..Default::default()
        });
        let (result, _) = e.check(ip("10.0.0.1"), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));
    }

    #[test]
    fn test_ip_blacklist_blocks() {
        let e = engine();
        let waf = waf_enabled(WafRules {
            ip_blacklist: vec![net("192.168.0.0/16")],
            ..Default::default()
        });
        let (result, _) = e.check(ip("192.168.1.1"), &waf, "site1");
        assert!(result.is_blocked());
        if let WafResult::Block { block_type, .. } = result {
            assert_eq!(block_type, "ip_blacklist");
        }
    }

    #[test]
    fn test_ip_not_in_blacklist_allows() {
        let e = engine();
        let waf = waf_enabled(WafRules {
            ip_blacklist: vec![net("192.168.0.0/16")],
            ..Default::default()
        });
        let (result, _) = e.check(ip("10.0.0.1"), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));
    }

    #[test]
    fn test_log_mode_does_not_block() {
        let e = engine();
        let waf = WafConfig {
            enabled: true,
            mode: WafMode::Log,
            rules: WafRules {
                ip_blacklist: vec![net("1.2.3.0/24")],
                ..Default::default()
            },
            ..Default::default()
        };
        let (result, _) = e.check(ip("1.2.3.4"), &waf, "site1");
        assert!(!result.is_blocked());
        assert!(matches!(result, WafResult::Log { .. }));
    }

    #[test]
    fn test_country_whitelist_fail_closed_no_geoip() {
        // Without GeoIP, country is unknown → fail-closed
        let e = engine();
        let waf = waf_enabled(WafRules {
            country_whitelist: vec!["US".to_string()],
            ..Default::default()
        });
        let (result, _) = e.check(ip("8.8.8.8"), &waf, "site1");
        assert!(result.is_blocked());
        if let WafResult::Block {
            block_type, reason, ..
        } = result
        {
            assert_eq!(block_type, "country_whitelist");
            assert!(reason.contains("unknown"));
        }
    }

    #[test]
    fn test_no_geo_rules_allows_without_lookup() {
        let e = engine();
        let waf = waf_enabled(WafRules::default());
        let (result, geo) = e.check(ip("8.8.8.8"), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));
        assert!(geo.is_none()); // No GeoIP lookup performed
    }

    #[test]
    fn test_empty_rules_allows() {
        let e = engine();
        let waf = waf_enabled(WafRules::default());
        let (result, _) = e.check(ip("1.2.3.4"), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));
    }

    #[test]
    fn test_whitelist_priority_over_blacklist() {
        // IP is in both whitelist and blacklist — whitelist takes priority (step 1 before step 2)
        let e = engine();
        let waf = waf_enabled(WafRules {
            ip_whitelist: vec![net("1.2.3.4/32")],
            ip_blacklist: vec![net("1.2.3.0/24")],
            ..Default::default()
        });
        let (result, _) = e.check(ip("1.2.3.4"), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));
    }

    #[test]
    fn test_region_blacklist_without_geoip_allows() {
        // Without GeoIP, region check can't match → allows
        let e = engine();
        let mut region_blacklist = HashMap::new();
        region_blacklist.insert("CN".to_string(), vec!["GD".to_string()]);
        let waf = waf_enabled(WafRules {
            region_blacklist,
            ..Default::default()
        });
        let (result, _) = e.check(ip("1.2.3.4"), &waf, "site1");
        // Without GeoIP, country_code is None, so region check is skipped
        assert!(matches!(result, WafResult::Allow));
    }

    #[test]
    fn test_ipv6_blacklist() {
        let e = engine();
        let waf = waf_enabled(WafRules {
            ip_blacklist: vec![net("2001:db8::/32")],
            ..Default::default()
        });
        let (result, _) = e.check(ip("2001:db8::1"), &waf, "site1");
        assert!(result.is_blocked());
    }
}
