use cdn_common::{UrlRedirectRule, UrlRuleType};
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
            // LRU promotion: move to back of queue on access
            if let Some(pos) = order.iter().position(|k| k == pattern) {
                order.remove(pos);
                order.push_back(pattern.to_string());
            }
            return Some(re.clone());
        }
        match Regex::new(pattern) {
            Ok(re) => {
                // LRU eviction: remove least-recently-used when at capacity
                while map.len() >= 256 {
                    if let Some(oldest) = order.pop_front() {
                        map.remove(&oldest);
                    }
                }
                order.push_back(pattern.to_string());
                map.insert(pattern.to_string(), re.clone());
                Some(re)
            }
            Err(e) => {
                log::warn!("[Redirect] invalid regex pattern {:?}: {}", pattern, e);
                None
            }
        }
    })
}

/// Result of a URL redirect rule match.
pub struct UrlRedirectResult {
    pub target_url: String,
    pub status_code: u16,
    pub cache_control: Option<String>,
    pub response_headers: std::collections::HashMap<String, String>,
}

/// Request context for variable substitution.
pub struct RequestInfo<'a> {
    pub scheme: &'a str,
    pub host: &'a str,
    pub uri: &'a str,
    pub path: &'a str,
    pub query_string: Option<&'a str>,
    pub method: &'a str,
}

/// Check URL redirect rules in order. Returns the first match.
pub fn check_url_rules(
    req: &RequestInfo<'_>,
    rules: &[UrlRedirectRule],
) -> Option<UrlRedirectResult> {
    for rule in rules {
        if !rule.enabled {
            continue;
        }

        // Method filter
        if !rule.methods.is_empty()
            && !rule
                .methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(req.method))
        {
            continue;
        }

        let source = rule.source.as_deref().unwrap_or("/");
        let match_input = if rule.match_query_string {
            req.uri
        } else {
            req.path
        };

        match rule.r#type {
            UrlRuleType::Exact => {
                if match_input == source {
                    let target = substitute_variables(&rule.target, req, &[]);
                    let target =
                        append_query_string(&target, req.query_string, rule.preserve_query_string);
                    return Some(make_result(target, rule));
                }
            }
            UrlRuleType::Prefix => {
                if let Some(remainder) = match_input.strip_prefix(source) {
                    let captures = vec![remainder.to_string()];
                    let target = substitute_variables(&rule.target, req, &captures);
                    let target =
                        append_query_string(&target, req.query_string, rule.preserve_query_string);
                    return Some(make_result(target, rule));
                }
            }
            UrlRuleType::Regex => {
                let flags = rule.regex_options.as_deref().unwrap_or("");
                let case_insensitive = flags.contains('i');
                let pattern = if case_insensitive {
                    format!("(?i){}", source)
                } else {
                    source.to_string()
                };
                if let Some(re) = get_or_compile_regex(&pattern) {
                    if let Some(caps) = re.captures(match_input) {
                        let captures: Vec<String> = (1..caps.len())
                            .map(|i| {
                                caps.get(i)
                                    .map(|m| m.as_str().to_string())
                                    .unwrap_or_default()
                            })
                            .collect();
                        let target = substitute_variables(&rule.target, req, &captures);
                        let target = append_query_string(
                            &target,
                            req.query_string,
                            rule.preserve_query_string,
                        );
                        return Some(make_result(target, rule));
                    }
                }
            }
            UrlRuleType::Domain => {
                let source_domain = rule.source_domain.as_deref().unwrap_or(source);
                if let Some(captures) = match_domain(req.host, source_domain) {
                    let target = substitute_variables(&rule.target, req, &captures);
                    let target =
                        append_query_string(&target, req.query_string, rule.preserve_query_string);
                    return Some(make_result(target, rule));
                }
            }
        }
    }
    None
}

/// Variable substitution in target URL.
/// Replaces: $request_uri, $query_string, $server_name, $scheme, $host, $uri, $args
/// Plus regex capture groups: $1, $2, ...
fn substitute_variables(target: &str, req: &RequestInfo<'_>, captures: &[String]) -> String {
    let mut result = target.to_string();

    // Replace capture groups in reverse order ($9, $8, ... $1)
    // to prevent $1 from consuming $10, $11, etc.
    for (i, cap) in captures.iter().enumerate().rev() {
        let var = format!("${}", i + 1);
        result = result.replace(&var, &sanitize_header_value(cap));
    }

    // Replace named variables (longer names first to avoid partial matches)
    // All user-controlled values are sanitized to prevent CRLF injection
    // in Location headers (HTTP response splitting).
    let qs = req.query_string.unwrap_or("");
    result = result.replace("$request_uri", &sanitize_header_value(req.uri));
    result = result.replace("$query_string", &sanitize_header_value(qs));
    result = result.replace("$server_name", &sanitize_header_value(req.host));
    result = result.replace("$scheme", req.scheme);
    result = result.replace("$host", &sanitize_header_value(req.host));
    result = result.replace("$uri", &sanitize_header_value(req.path));
    result = result.replace("$args", &sanitize_header_value(qs));

    result
}

/// Strip characters that are dangerous in HTTP header values (CR, LF, NUL).
fn sanitize_header_value(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
        .collect()
}

/// Append query string to target URL if preserve_query_string is enabled.
fn append_query_string(target: &str, query_string: Option<&str>, preserve: bool) -> String {
    if !preserve {
        return target.to_string();
    }
    let qs = match query_string {
        Some(q) if !q.is_empty() => q,
        _ => return target.to_string(),
    };

    if target.contains('?') {
        format!("{}&{}", target, qs)
    } else {
        format!("{}?{}", target, qs)
    }
}

/// Match a host against a domain pattern (supports `*.example.com` wildcards).
/// Returns capture groups: $1 = subdomain part for wildcards.
fn match_domain(host: &str, pattern: &str) -> Option<Vec<String>> {
    let host_no_port = host.split(':').next().unwrap_or(host);

    if let Some(suffix) = pattern.strip_prefix("*.") {
        let suffix_lower = suffix.to_ascii_lowercase();
        let host_lower = host_no_port.to_ascii_lowercase();
        if host_lower.ends_with(&suffix_lower)
            && host_lower.len() > suffix_lower.len()
            && host_lower.as_bytes()[host_lower.len() - suffix_lower.len() - 1] == b'.'
        {
            let subdomain = &host_no_port[..host_no_port.len() - suffix.len() - 1];
            return Some(vec![subdomain.to_string()]);
        }
    } else if host_no_port.eq_ignore_ascii_case(pattern) {
        return Some(vec![]);
    }

    None
}

fn make_result(target_url: String, rule: &UrlRedirectRule) -> UrlRedirectResult {
    UrlRedirectResult {
        target_url,
        status_code: rule.status_code,
        cache_control: rule.cache_control.clone(),
        response_headers: rule.response_headers.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn req<'a>(
        scheme: &'a str,
        host: &'a str,
        uri: &'a str,
        path: &'a str,
        qs: Option<&'a str>,
    ) -> RequestInfo<'a> {
        RequestInfo {
            scheme,
            host,
            uri,
            path,
            query_string: qs,
            method: "GET",
        }
    }

    fn rule_exact(source: &str, target: &str) -> UrlRedirectRule {
        UrlRedirectRule {
            r#type: UrlRuleType::Exact,
            source: Some(source.to_string()),
            source_domain: None,
            target: target.to_string(),
            status_code: 301,
            enabled: true,
            preserve_query_string: true,
            methods: vec![],
            match_query_string: false,
            regex_options: None,
            cache_control: None,
            response_headers: HashMap::new(),
        }
    }

    fn rule_prefix(source: &str, target: &str) -> UrlRedirectRule {
        UrlRedirectRule {
            r#type: UrlRuleType::Prefix,
            source: Some(source.to_string()),
            ..rule_exact(source, target)
        }
    }

    fn rule_regex(source: &str, target: &str) -> UrlRedirectRule {
        UrlRedirectRule {
            r#type: UrlRuleType::Regex,
            source: Some(source.to_string()),
            ..rule_exact(source, target)
        }
    }

    fn rule_domain(source_domain: &str, target: &str) -> UrlRedirectRule {
        UrlRedirectRule {
            r#type: UrlRuleType::Domain,
            source: None,
            source_domain: Some(source_domain.to_string()),
            ..rule_exact("/", target)
        }
    }

    // ── Exact match ──

    #[test]
    fn test_exact_match() {
        let rules = vec![rule_exact("/old", "/new")];
        let r = req("https", "example.com", "/old", "/old", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/new");
    }

    #[test]
    fn test_exact_no_match() {
        let rules = vec![rule_exact("/old", "/new")];
        let r = req("https", "example.com", "/other", "/other", None);
        assert!(check_url_rules(&r, &rules).is_none());
    }

    #[test]
    fn test_exact_preserves_query() {
        let rules = vec![rule_exact("/old", "/new")];
        let r = req("https", "example.com", "/old?a=1", "/old", Some("a=1"));
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/new?a=1");
    }

    // ── Prefix match ──

    #[test]
    fn test_prefix_match() {
        let rules = vec![rule_prefix("/blog", "/articles$1")];
        let r = req("https", "example.com", "/blog/post-1", "/blog/post-1", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/articles/post-1");
    }

    #[test]
    fn test_prefix_root() {
        let rules = vec![rule_prefix("/old/", "/new/")];
        let r = req("https", "example.com", "/old/page", "/old/page", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/new/");
    }

    // ── Regex match ──

    #[test]
    fn test_regex_match() {
        let rules = vec![rule_regex(r"^/user/(\d+)", "/profile/$1")];
        let r = req("https", "example.com", "/user/123", "/user/123", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/profile/123");
    }

    #[test]
    fn test_regex_multiple_captures() {
        let rules = vec![rule_regex(r"^/(\w+)/(\d+)", "/$1/item/$2")];
        let r = req("https", "example.com", "/blog/42", "/blog/42", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/blog/item/42");
    }

    #[test]
    fn test_regex_case_insensitive() {
        let mut rule = rule_regex(r"^/OLD", "/new");
        rule.regex_options = Some("i".to_string());
        let rules = vec![rule];
        let r = req("https", "example.com", "/old", "/old", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/new");
    }

    // ── Domain match ──

    #[test]
    fn test_domain_exact() {
        let rules = vec![rule_domain("old.com", "https://new.com$uri")];
        let r = req("https", "old.com", "/path", "/path", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "https://new.com/path");
    }

    #[test]
    fn test_domain_wildcard() {
        let rules = vec![rule_domain("*.old.com", "https://$1.new.com$uri")];
        let r = req("https", "sub.old.com", "/path", "/path", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "https://sub.new.com/path");
    }

    // ── Variable substitution ──

    #[test]
    fn test_variables() {
        let rules = vec![rule_exact("/old", "$scheme://$host/new")];
        let r = req("https", "example.com", "/old", "/old", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "https://example.com/new");
    }

    // ── Method filter ──

    #[test]
    fn test_method_filter() {
        let mut rule = rule_exact("/api", "/new-api");
        rule.methods = vec!["POST".to_string()];
        let rules = vec![rule];

        let r = RequestInfo {
            scheme: "https",
            host: "example.com",
            uri: "/api",
            path: "/api",
            query_string: None,
            method: "GET",
        };
        assert!(check_url_rules(&r, &rules).is_none());

        let r = RequestInfo {
            method: "POST",
            ..r
        };
        assert!(check_url_rules(&r, &rules).is_some());
    }

    // ── Disabled rule ──

    #[test]
    fn test_disabled_rule_skipped() {
        let mut rule = rule_exact("/old", "/new");
        rule.enabled = false;
        let rules = vec![rule];
        let r = req("https", "example.com", "/old", "/old", None);
        assert!(check_url_rules(&r, &rules).is_none());
    }

    // ── No preserve query string ──

    #[test]
    fn test_no_preserve_query() {
        let mut rule = rule_exact("/old", "/new");
        rule.preserve_query_string = false;
        let rules = vec![rule];
        let r = req("https", "example.com", "/old?a=1", "/old", Some("a=1"));
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/new");
    }

    // ── First match wins ──

    #[test]
    fn test_first_match_wins() {
        let rules = vec![
            rule_exact("/path", "/first"),
            rule_exact("/path", "/second"),
        ];
        let r = req("https", "example.com", "/path", "/path", None);
        let result = check_url_rules(&r, &rules).unwrap();
        assert_eq!(result.target_url, "/first");
    }

    // ── Empty rules ──

    #[test]
    fn test_empty_rules() {
        let r = req("https", "example.com", "/path", "/path", None);
        assert!(check_url_rules(&r, &[]).is_none());
    }

    #[test]
    fn test_crlf_sanitized_in_variables() {
        let rules = vec![rule_exact("/old", "$scheme://$host$uri")];
        // Host contains CRLF — should be stripped to prevent HTTP response splitting
        let r = RequestInfo {
            scheme: "https",
            host: "evil.com\r\nX-Injected: true",
            uri: "/old",
            path: "/old",
            query_string: None,
            method: "GET",
        };
        let result = check_url_rules(&r, &rules).unwrap();
        assert!(!result.target_url.contains('\r'));
        assert!(!result.target_url.contains('\n'));
        assert_eq!(result.target_url, "https://evil.comX-Injected: true/old");
    }
}
