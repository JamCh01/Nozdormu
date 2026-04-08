use crate::ssl::storage::{CertData, CertStorage};
use moka::future::Cache;
use std::sync::Arc;
use std::time::Duration;

/// Dynamic certificate manager.
///
/// Lookup priority:
/// 1. moka cache (TTL 300s)
/// 2. CertStorage (file/etcd)
/// 3. Wildcard match (*.example.com)
/// 4. Default certificate
pub struct CertManager {
    storage: Arc<CertStorage>,
    /// In-memory cache with 5-minute TTL
    cache: Cache<String, Option<CertData>>,
}

impl CertManager {
    pub fn new(storage: Arc<CertStorage>) -> Self {
        let cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build();

        Self { storage, cache }
    }

    /// Look up a certificate for the given SNI domain.
    ///
    /// Priority: exact match → wildcard → default
    pub async fn get_cert(&self, domain: &str) -> Option<CertData> {
        let domain = domain.to_ascii_lowercase();

        // Check moka cache
        if let Some(cached) = self.cache.get(&domain).await {
            return cached;
        }

        // Exact match from storage
        if let Some(cert) = self.storage.get(&domain) {
            if !cert.is_expired() {
                self.cache.insert(domain, Some(cert.clone())).await;
                return Some(cert);
            }
            log::warn!("[CertManager] expired cert for {}", domain);
        }

        // Wildcard match
        if let Some(cert) = self.storage.get_wildcard(&domain) {
            if !cert.is_expired() {
                self.cache.insert(domain, Some(cert.clone())).await;
                return Some(cert);
            }
        }

        // Default certificate
        if let Some(cert) = self.storage.get_default() {
            if !cert.is_expired() {
                self.cache.insert(domain, Some(cert.clone())).await;
                return Some(cert);
            }
        }

        // Cache the miss to avoid repeated lookups
        self.cache.insert(domain, None).await;
        None
    }

    /// Invalidate the cache for a domain (e.g., after renewal).
    pub async fn invalidate(&self, domain: &str) {
        self.cache.remove(domain).await;
    }

    /// Clear the entire cache.
    pub async fn clear_cache(&self) {
        self.cache.invalidate_all();
    }

    /// Get a reference to the underlying storage.
    pub fn storage(&self) -> &CertStorage {
        &self.storage
    }
}

/// Match a domain against a wildcard pattern per RFC 6125.
/// Only single-level wildcards are supported: *.example.com matches foo.example.com
/// but NOT foo.bar.example.com.
pub fn matches_wildcard(domain: &str, pattern: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // Must have exactly one level before the suffix
        if let Some(dot_pos) = domain.find('.') {
            let rest = &domain[dot_pos + 1..];
            return rest.eq_ignore_ascii_case(suffix) && !domain[..dot_pos].contains('.');
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wildcard_match() {
        assert!(matches_wildcard("foo.example.com", "*.example.com"));
        assert!(matches_wildcard("bar.example.com", "*.example.com"));
    }

    #[test]
    fn test_wildcard_no_multi_level() {
        // RFC 6125: wildcard only matches single level
        assert!(!matches_wildcard("foo.bar.example.com", "*.example.com"));
    }

    #[test]
    fn test_wildcard_no_match() {
        assert!(!matches_wildcard("example.com", "*.example.com"));
        assert!(!matches_wildcard("other.com", "*.example.com"));
    }

    #[test]
    fn test_wildcard_case_insensitive() {
        assert!(matches_wildcard("FOO.Example.COM", "*.example.com"));
    }
}
