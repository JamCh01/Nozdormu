use cdn_common::{CcKeyType, CcRule};
use std::net::IpAddr;

/// Generate a CC counter key based on the key type.
///
/// | Type    | Key Format                                  | Granularity          |
/// |---------|---------------------------------------------|----------------------|
/// | ip      | `nozdormu:cc:counter:{site}:{ip}`           | Site-wide rate limit |
/// | ip_url  | `...:{ip}:{crc32(uri)}`                     | Per-URL (with query) |
/// | ip_path | `...:{ip}:{crc32(path)}`                    | Per-path (no query)  |
pub fn make_counter_key(
    site_id: &str,
    ip: IpAddr,
    uri: &str,
    path: &str,
    key_type: &CcKeyType,
) -> String {
    let prefix = format!("nozdormu:cc:counter:{}:{}", site_id, ip);
    match key_type {
        CcKeyType::Ip => prefix,
        CcKeyType::IpUrl => {
            let hash = crc32fast::hash(uri.as_bytes());
            format!("{}:{:08x}", prefix, hash)
        }
        CcKeyType::IpPath => {
            let hash = crc32fast::hash(path.as_bytes());
            format!("{}:{:08x}", prefix, hash)
        }
    }
}

/// Match the most specific CC rule for a given URI path.
/// Uses longest path prefix matching.
/// Returns None if no rule matches (caller should use site defaults).
pub fn match_rule<'a>(path: &str, rules: &'a [CcRule]) -> Option<&'a CcRule> {
    rules
        .iter()
        .filter(|r| path.starts_with(&r.path))
        .max_by_key(|r| r.path.len())
}

/// Determine if this count should trigger an async Redis sync.
/// Syncs at count 10, then every 10 increments (20, 30, 40...).
/// This reduces Redis pressure while keeping distributed counts roughly accurate.
pub fn should_sync_redis(count: u64) -> bool {
    count >= 10 && count % 10 == 0
}

/// Build the Redis counter key (same format as local key).
pub fn make_redis_key(local_key: &str) -> String {
    // Redis key uses the same format as local key
    local_key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdn_common::CcAction;

    fn rule(path: &str, rate: u64) -> CcRule {
        CcRule {
            path: path.to_string(),
            rate,
            window: 60,
            block_duration: 600,
            action: CcAction::Block,
            key_type: CcKeyType::IpUrl,
        }
    }

    #[test]
    fn test_match_rule_longest_prefix() {
        let rules = vec![rule("/", 100), rule("/api", 50), rule("/api/v1", 30)];
        let matched = match_rule("/api/v1/users", &rules).unwrap();
        assert_eq!(matched.path, "/api/v1");
        assert_eq!(matched.rate, 30);
    }

    #[test]
    fn test_match_rule_root() {
        let rules = vec![rule("/", 100), rule("/api", 50)];
        let matched = match_rule("/other/path", &rules).unwrap();
        assert_eq!(matched.path, "/");
    }

    #[test]
    fn test_match_rule_no_match() {
        let rules = vec![rule("/api", 50)];
        assert!(match_rule("/other", &rules).is_none());
    }

    #[test]
    fn test_match_rule_empty() {
        let rules: Vec<CcRule> = vec![];
        assert!(match_rule("/anything", &rules).is_none());
    }

    #[test]
    fn test_counter_key_ip() {
        let key = make_counter_key(
            "site1",
            "1.2.3.4".parse().unwrap(),
            "/api?q=1",
            "/api",
            &CcKeyType::Ip,
        );
        assert_eq!(key, "nozdormu:cc:counter:site1:1.2.3.4");
    }

    #[test]
    fn test_counter_key_ip_url() {
        let key = make_counter_key(
            "site1",
            "1.2.3.4".parse().unwrap(),
            "/api?q=1",
            "/api",
            &CcKeyType::IpUrl,
        );
        assert!(key.starts_with("nozdormu:cc:counter:site1:1.2.3.4:"));
        // Different URIs should produce different keys
        let key2 = make_counter_key(
            "site1",
            "1.2.3.4".parse().unwrap(),
            "/api?q=2",
            "/api",
            &CcKeyType::IpUrl,
        );
        assert_ne!(key, key2);
    }

    #[test]
    fn test_counter_key_ip_path() {
        let key1 = make_counter_key(
            "site1",
            "1.2.3.4".parse().unwrap(),
            "/api?q=1",
            "/api",
            &CcKeyType::IpPath,
        );
        let key2 = make_counter_key(
            "site1",
            "1.2.3.4".parse().unwrap(),
            "/api?q=2",
            "/api",
            &CcKeyType::IpPath,
        );
        // Same path, different query → same key for ip_path
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_should_sync_redis() {
        assert!(!should_sync_redis(1));
        assert!(!should_sync_redis(5));
        assert!(!should_sync_redis(9));
        assert!(should_sync_redis(10));
        assert!(!should_sync_redis(11));
        assert!(should_sync_redis(20));
        assert!(should_sync_redis(100));
    }
}
