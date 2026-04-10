use cdn_common::{CacheConfig, CacheRule, CacheRuleType};
use regex::Regex;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

thread_local! {
    static REGEX_CACHE: RefCell<(HashMap<String, Regex>, VecDeque<String>)> =
        RefCell::new((HashMap::new(), VecDeque::new()));
}

fn get_or_compile_regex(pattern: &str) -> Option<Regex> {
    REGEX_CACHE.with(|cache| {
        let (ref mut map, ref mut order) = *cache.borrow_mut();
        if let Some(re) = map.get(pattern) {
            return Some(re.clone());
        }
        match Regex::new(pattern) {
            Ok(re) => {
                // LRU eviction: remove oldest entries when at capacity
                while map.len() >= 256 {
                    if let Some(oldest) = order.pop_front() {
                        map.remove(&oldest);
                    }
                }
                order.push_back(pattern.to_string());
                map.insert(pattern.to_string(), re.clone());
                Some(re)
            }
            Err(_) => None,
        }
    })
}

/// Result of cache strategy evaluation.
#[derive(Debug, Clone)]
pub struct CacheDecision {
    /// Whether the request/response is cacheable.
    pub cacheable: bool,
    /// TTL in seconds (0 = not cacheable).
    pub ttl: u64,
    /// Reason for the decision (for logging).
    pub reason: &'static str,
}

impl CacheDecision {
    fn skip(reason: &'static str) -> Self {
        Self {
            cacheable: false,
            ttl: 0,
            reason,
        }
    }

    fn cache(ttl: u64, reason: &'static str) -> Self {
        Self {
            cacheable: true,
            ttl,
            reason,
        }
    }
}

/// Check if a request is cacheable and determine TTL from rules.
///
/// Checks:
/// 1. cache.enabled
/// 2. Only GET/HEAD methods
/// 3. Request Cache-Control: no-cache/no-store → skip
/// 4. Authorization header → skip (unless cache_authorized)
/// 5. Match cache rules for TTL
/// 6. TTL <= 0 → skip
pub fn check_request_cacheability(
    method: &str,
    path: &str,
    cache_control: Option<&str>,
    has_authorization: bool,
    config: &CacheConfig,
) -> CacheDecision {
    if !config.enabled {
        return CacheDecision::skip("cache disabled");
    }

    // Only GET/HEAD
    if !matches!(method, "GET" | "HEAD") {
        return CacheDecision::skip("method not cacheable");
    }

    // Request Cache-Control
    if let Some(cc) = cache_control {
        let cc_lower = cc.to_ascii_lowercase();
        if cc_lower.contains("no-cache") || cc_lower.contains("no-store") {
            return CacheDecision::skip("request cache-control");
        }
    }

    // Authorization header
    if has_authorization && !config.cache_authorized {
        return CacheDecision::skip("authorization present");
    }

    // Match rules for TTL
    let ttl = match_rule_ttl(path, &config.rules, config.default_ttl);
    if ttl == 0 {
        return CacheDecision::skip("ttl is zero");
    }

    CacheDecision::cache(ttl, "rule match")
}

/// Check if a response is cacheable.
///
/// Checks:
/// - Cacheable status codes: 200, 203, 204, 206, 300, 301, 302, 304, 307, 308, 404, 410
/// - Cache-Control: private/no-cache/no-store → skip
/// - Set-Cookie → skip (unless cache_cookies)
/// - Vary: * → skip
/// - Content-Length > max_size → skip
pub fn check_response_cacheability(
    status: u16,
    cache_control: Option<&str>,
    has_set_cookie: bool,
    vary: Option<&str>,
    content_length: Option<u64>,
    config: &CacheConfig,
) -> CacheDecision {
    // Status code check
    const CACHEABLE_STATUSES: &[u16] =
        &[200, 203, 204, 206, 300, 301, 302, 304, 307, 308, 404, 410];
    if !CACHEABLE_STATUSES.contains(&status) {
        return CacheDecision::skip("status not cacheable");
    }

    // Response Cache-Control
    if let Some(cc) = cache_control {
        let cc_lower = cc.to_ascii_lowercase();
        if cc_lower.contains("private")
            || cc_lower.contains("no-cache")
            || cc_lower.contains("no-store")
        {
            return CacheDecision::skip("response cache-control");
        }
    }

    // Set-Cookie
    if has_set_cookie && !config.cache_cookies {
        return CacheDecision::skip("set-cookie present");
    }

    // Vary: *
    if let Some(v) = vary {
        if v.trim() == "*" {
            return CacheDecision::skip("vary: *");
        }
    }

    // Content-Length > max_size
    if let Some(len) = content_length {
        if len > config.max_size {
            return CacheDecision::skip("content too large");
        }
    }

    CacheDecision::cache(0, "response cacheable") // TTL determined by request phase
}

/// Adjust TTL based on response headers.
///
/// Priority: s-maxage > max-age > Expires > config TTL
/// Takes the minimum of response-derived TTL and config TTL.
pub fn adjust_ttl(config_ttl: u64, cache_control: Option<&str>, expires: Option<&str>) -> u64 {
    if let Some(cc) = cache_control {
        let cc_lower = cc.to_ascii_lowercase();
        // s-maxage (highest priority)
        if let Some(sma) = parse_directive(&cc_lower, "s-maxage") {
            return sma.min(config_ttl);
        }
        // max-age
        if let Some(ma) = parse_directive(&cc_lower, "max-age") {
            return ma.min(config_ttl);
        }
    }

    // Expires header (RFC 7234)
    if let Some(expires_str) = expires {
        if let Ok(expires_time) = chrono::DateTime::parse_from_rfc2822(expires_str) {
            let now = chrono::Utc::now();
            let diff = expires_time.signed_duration_since(now).num_seconds();
            if diff > 0 {
                return (diff as u64).min(config_ttl);
            }
            return 0; // Already expired
        }
    }

    config_ttl
}

/// Match cache rules to determine TTL.
/// Priority: path > extension > regex > default_ttl
fn match_rule_ttl(path: &str, rules: &[CacheRule], default_ttl: u64) -> u64 {
    // Path rules (longest prefix match)
    let mut best_path_match: Option<(usize, u64)> = None;
    for rule in rules {
        if rule.r#type != CacheRuleType::Path {
            continue;
        }
        if let Some(pattern) = rule.r#match.as_str() {
            if path.starts_with(pattern) {
                let len = pattern.len();
                if best_path_match.map(|(l, _)| len > l).unwrap_or(true) {
                    best_path_match = Some((len, rule_ttl_seconds(rule)));
                }
            }
        }
    }
    if let Some((_, ttl)) = best_path_match {
        return ttl;
    }

    // Extension rules — extract file extension from the last path segment.
    // rsplit('.').next() returns the whole string when there's no dot, so we
    // check that the split actually found a dot by comparing lengths.
    let ext = match path.rsplit_once('.') {
        Some((_, after)) => after,
        None => "",
    };
    for rule in rules {
        if rule.r#type != CacheRuleType::Extension {
            continue;
        }
        if let Some(extensions) = rule.r#match.as_array() {
            for e in extensions {
                if let Some(e_str) = e.as_str() {
                    if e_str.eq_ignore_ascii_case(ext) {
                        return rule_ttl_seconds(rule);
                    }
                }
            }
        } else if let Some(e_str) = rule.r#match.as_str() {
            if e_str.eq_ignore_ascii_case(ext) {
                return rule_ttl_seconds(rule);
            }
        }
    }

    // Regex rules
    for rule in rules {
        if rule.r#type != CacheRuleType::Regex {
            continue;
        }
        if let Some(pattern) = rule.r#match.as_str() {
            let flags = rule.regex_options.as_deref().unwrap_or("");
            let pattern = if flags.contains('i') {
                format!("(?i){}", pattern)
            } else {
                pattern.to_string()
            };
            if let Some(re) = get_or_compile_regex(&pattern) {
                if re.is_match(path) {
                    return rule_ttl_seconds(rule);
                }
            }
        }
    }

    default_ttl
}

/// Convert rule TTL to seconds based on ttl_unit.
fn rule_ttl_seconds(rule: &CacheRule) -> u64 {
    match rule.ttl_unit.as_str() {
        "minutes" => rule.ttl * 60,
        "hours" => rule.ttl * 3600,
        "days" => rule.ttl * 86400,
        _ => rule.ttl, // "seconds" or default
    }
}

/// Parse a numeric directive from Cache-Control header.
fn parse_directive(cache_control: &str, directive: &str) -> Option<u64> {
    let prefix = format!("{}=", directive);
    for part in cache_control.split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&prefix) {
            if let Ok(v) = value.trim().parse::<u64>() {
                return Some(v);
            }
        }
    }
    None
}

/// Parse the `stale-while-revalidate` directive from Cache-Control.
/// Returns 0 if not present.
pub fn parse_stale_while_revalidate(cache_control: Option<&str>) -> u64 {
    cache_control
        .and_then(|cc| parse_directive(&cc.to_ascii_lowercase(), "stale-while-revalidate"))
        .unwrap_or(0)
}

/// Headers that should be excluded from cached responses.
pub const EXCLUDED_RESPONSE_HEADERS: &[&str] = &[
    "set-cookie",
    "x-cache-status",
    "age",
    "connection",
    "keep-alive",
    "transfer-encoding",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn default_config() -> CacheConfig {
        CacheConfig::default()
    }

    fn config_with_rules(rules: Vec<CacheRule>) -> CacheConfig {
        CacheConfig {
            rules,
            ..Default::default()
        }
    }

    // ── Request cacheability ──

    #[test]
    fn test_get_is_cacheable() {
        let d = check_request_cacheability("GET", "/page", None, false, &default_config());
        assert!(d.cacheable);
    }

    #[test]
    fn test_head_is_cacheable() {
        let d = check_request_cacheability("HEAD", "/page", None, false, &default_config());
        assert!(d.cacheable);
    }

    #[test]
    fn test_post_not_cacheable() {
        let d = check_request_cacheability("POST", "/api", None, false, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_no_cache_header() {
        let d = check_request_cacheability("GET", "/", Some("no-cache"), false, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_no_store_header() {
        let d = check_request_cacheability("GET", "/", Some("no-store"), false, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_authorization_skips() {
        let d = check_request_cacheability("GET", "/", None, true, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_authorization_allowed() {
        let mut cfg = default_config();
        cfg.cache_authorized = true;
        let d = check_request_cacheability("GET", "/", None, true, &cfg);
        assert!(d.cacheable);
    }

    #[test]
    fn test_disabled_cache() {
        let mut cfg = default_config();
        cfg.enabled = false;
        let d = check_request_cacheability("GET", "/", None, false, &cfg);
        assert!(!d.cacheable);
    }

    // ── Response cacheability ──

    #[test]
    fn test_200_cacheable() {
        let d = check_response_cacheability(200, None, false, None, None, &default_config());
        assert!(d.cacheable);
    }

    #[test]
    fn test_500_not_cacheable() {
        let d = check_response_cacheability(500, None, false, None, None, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_private_not_cacheable() {
        let d =
            check_response_cacheability(200, Some("private"), false, None, None, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_set_cookie_not_cacheable() {
        let d = check_response_cacheability(200, None, true, None, None, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_set_cookie_allowed() {
        let mut cfg = default_config();
        cfg.cache_cookies = true;
        let d = check_response_cacheability(200, None, true, None, None, &cfg);
        assert!(d.cacheable);
    }

    #[test]
    fn test_vary_star_not_cacheable() {
        let d = check_response_cacheability(200, None, false, Some("*"), None, &default_config());
        assert!(!d.cacheable);
    }

    #[test]
    fn test_too_large_not_cacheable() {
        let d = check_response_cacheability(
            200,
            None,
            false,
            None,
            Some(200_000_000),
            &default_config(),
        );
        assert!(!d.cacheable);
    }

    // ── Rule matching ──

    #[test]
    fn test_path_rule() {
        let rules = vec![CacheRule {
            r#type: CacheRuleType::Path,
            r#match: json!("/static"),
            ttl: 7200,
            ttl_unit: "seconds".to_string(),
            regex_options: None,
        }];
        let cfg = config_with_rules(rules);
        let d = check_request_cacheability("GET", "/static/file.js", None, false, &cfg);
        assert!(d.cacheable);
        assert_eq!(d.ttl, 7200);
    }

    #[test]
    fn test_extension_rule() {
        let rules = vec![CacheRule {
            r#type: CacheRuleType::Extension,
            r#match: json!(["js", "css", "png"]),
            ttl: 86400,
            ttl_unit: "seconds".to_string(),
            regex_options: None,
        }];
        let cfg = config_with_rules(rules);
        let d = check_request_cacheability("GET", "/app.js", None, false, &cfg);
        assert!(d.cacheable);
        assert_eq!(d.ttl, 86400);
    }

    #[test]
    fn test_regex_rule() {
        let rules = vec![CacheRule {
            r#type: CacheRuleType::Regex,
            r#match: json!(r"^/api/v\d+/public"),
            ttl: 300,
            ttl_unit: "seconds".to_string(),
            regex_options: None,
        }];
        let cfg = config_with_rules(rules);
        let d = check_request_cacheability("GET", "/api/v2/public/data", None, false, &cfg);
        assert!(d.cacheable);
        assert_eq!(d.ttl, 300);
    }

    #[test]
    fn test_default_ttl() {
        let cfg = default_config();
        let d = check_request_cacheability("GET", "/page", None, false, &cfg);
        assert!(d.cacheable);
        assert_eq!(d.ttl, 3600); // default_cache_ttl
    }

    #[test]
    fn test_ttl_unit_hours() {
        let rules = vec![CacheRule {
            r#type: CacheRuleType::Path,
            r#match: json!("/"),
            ttl: 2,
            ttl_unit: "hours".to_string(),
            regex_options: None,
        }];
        let cfg = config_with_rules(rules);
        let d = check_request_cacheability("GET", "/page", None, false, &cfg);
        assert_eq!(d.ttl, 7200);
    }

    // ── TTL adjustment ──

    #[test]
    fn test_s_maxage_priority() {
        let ttl = adjust_ttl(3600, Some("max-age=600, s-maxage=300"), None);
        assert_eq!(ttl, 300);
    }

    #[test]
    fn test_max_age() {
        let ttl = adjust_ttl(3600, Some("max-age=600"), None);
        assert_eq!(ttl, 600);
    }

    #[test]
    fn test_ttl_capped_by_config() {
        let ttl = adjust_ttl(100, Some("max-age=600"), None);
        assert_eq!(ttl, 100); // config TTL is smaller
    }

    #[test]
    fn test_no_headers_uses_config() {
        let ttl = adjust_ttl(3600, None, None);
        assert_eq!(ttl, 3600);
    }

    #[test]
    fn test_parse_directive() {
        assert_eq!(parse_directive("max-age=300, public", "max-age"), Some(300));
        assert_eq!(parse_directive("s-maxage=60", "s-maxage"), Some(60));
        assert_eq!(parse_directive("public", "max-age"), None);
    }

    #[test]
    fn test_adjust_ttl_case_insensitive() {
        // Max-Age (capitalized) should be recognized
        let ttl = adjust_ttl(3600, Some("Max-Age=600"), None);
        assert_eq!(ttl, 600);
        // S-MAXAGE (all caps)
        let ttl = adjust_ttl(3600, Some("S-MAXAGE=120, public"), None);
        assert_eq!(ttl, 120);
    }

    #[test]
    fn test_extension_rule_no_dot_in_path() {
        // Path with no dot should NOT match any extension rule
        let rules = vec![CacheRule {
            r#type: CacheRuleType::Extension,
            r#match: json!(["js", "css"]),
            ttl: 86400,
            ttl_unit: "seconds".to_string(),
            regex_options: None,
        }];
        let cfg = config_with_rules(rules);
        // "/api/data" has no extension — should fall through to default TTL, not match
        let d = check_request_cacheability("GET", "/api/data", None, false, &cfg);
        assert!(d.cacheable);
        assert_eq!(d.ttl, cfg.default_ttl);
    }

    // ── Stale-While-Revalidate parsing ──

    #[test]
    fn test_swr_present() {
        let swr = parse_stale_while_revalidate(Some("max-age=300, stale-while-revalidate=60"));
        assert_eq!(swr, 60);
    }

    #[test]
    fn test_swr_absent() {
        let swr = parse_stale_while_revalidate(Some("max-age=300"));
        assert_eq!(swr, 0);
    }

    #[test]
    fn test_swr_none() {
        let swr = parse_stale_while_revalidate(None);
        assert_eq!(swr, 0);
    }

    #[test]
    fn test_swr_case_insensitive() {
        let swr =
            parse_stale_while_revalidate(Some("Max-Age=300, Stale-While-Revalidate=120"));
        assert_eq!(swr, 120);
    }
}
