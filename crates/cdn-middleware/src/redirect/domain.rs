use cdn_common::DomainRedirectConfig;

/// Result of a domain redirect check.
pub struct DomainRedirectResult {
    pub target_url: String,
    pub status_code: u16,
}

/// Check if the request host should be redirected to a different domain.
///
/// Rules:
/// - If host is already the target domain → no redirect
/// - If source_domains is empty → all non-target domains redirect
/// - If source_domains is set → only listed domains redirect (supports `*.old.com` wildcards)
/// - Preserves original scheme and path+query
pub fn check_domain_redirect(
    host: &str,
    scheme: &str,
    uri: &str,
    config: &DomainRedirectConfig,
) -> Option<DomainRedirectResult> {
    if !config.enabled {
        return None;
    }

    // Strip port from host for comparison
    let host_no_port = host.split(':').next().unwrap_or(host);
    let target = &config.target_domain;

    // Already on target domain → no redirect
    if host_no_port.eq_ignore_ascii_case(target) {
        return None;
    }

    // Check if host matches source_domains
    if !config.source_domains.is_empty() {
        let matched = config.source_domains.iter().any(|src| {
            if let Some(suffix) = src.strip_prefix("*.") {
                // Wildcard: *.old.com matches sub.old.com, a.b.old.com
                let suffix_lower = suffix.to_ascii_lowercase();
                let host_lower = host_no_port.to_ascii_lowercase();
                host_lower.ends_with(&suffix_lower)
                    && host_lower.len() > suffix_lower.len()
                    && host_lower.as_bytes()[host_lower.len() - suffix_lower.len() - 1] == b'.'
            } else {
                host_no_port.eq_ignore_ascii_case(src)
            }
        });
        if !matched {
            return None;
        }
    }

    // Build redirect URL preserving scheme and path
    let target_url = format!("{}://{}{}", scheme, target, uri);

    Some(DomainRedirectResult {
        target_url,
        status_code: config.status_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(target: &str, sources: Vec<&str>, enabled: bool) -> DomainRedirectConfig {
        DomainRedirectConfig {
            enabled,
            target_domain: target.to_string(),
            source_domains: sources.into_iter().map(|s| s.to_string()).collect(),
            status_code: 301,
        }
    }

    #[test]
    fn test_disabled() {
        let cfg = config("new.com", vec![], false);
        assert!(check_domain_redirect("old.com", "https", "/path", &cfg).is_none());
    }

    #[test]
    fn test_already_on_target() {
        let cfg = config("new.com", vec![], true);
        assert!(check_domain_redirect("new.com", "https", "/path", &cfg).is_none());
    }

    #[test]
    fn test_already_on_target_case_insensitive() {
        let cfg = config("New.Com", vec![], true);
        assert!(check_domain_redirect("new.com", "https", "/", &cfg).is_none());
    }

    #[test]
    fn test_redirect_all_non_target() {
        let cfg = config("new.com", vec![], true);
        let result = check_domain_redirect("old.com", "https", "/path?q=1", &cfg).unwrap();
        assert_eq!(result.target_url, "https://new.com/path?q=1");
        assert_eq!(result.status_code, 301);
    }

    #[test]
    fn test_redirect_specific_source() {
        let cfg = config("new.com", vec!["old.com"], true);
        let result = check_domain_redirect("old.com", "https", "/", &cfg).unwrap();
        assert_eq!(result.target_url, "https://new.com/");

        // other.com should NOT redirect
        assert!(check_domain_redirect("other.com", "https", "/", &cfg).is_none());
    }

    #[test]
    fn test_wildcard_source() {
        let cfg = config("new.com", vec!["*.old.com"], true);

        let result = check_domain_redirect("sub.old.com", "https", "/", &cfg).unwrap();
        assert_eq!(result.target_url, "https://new.com/");

        let result = check_domain_redirect("a.b.old.com", "http", "/x", &cfg).unwrap();
        assert_eq!(result.target_url, "http://new.com/x");

        // old.com itself should NOT match *.old.com
        assert!(check_domain_redirect("old.com", "https", "/", &cfg).is_none());
    }

    #[test]
    fn test_preserves_scheme() {
        let cfg = config("new.com", vec![], true);
        let result = check_domain_redirect("old.com", "http", "/path", &cfg).unwrap();
        assert_eq!(result.target_url, "http://new.com/path");
    }

    #[test]
    fn test_host_with_port() {
        let cfg = config("new.com", vec![], true);
        let result = check_domain_redirect("old.com:8080", "https", "/", &cfg).unwrap();
        assert_eq!(result.target_url, "https://new.com/");
    }
}
