pub mod domain;
pub mod protocol;
pub mod url;

use cdn_common::{DomainRedirectConfig, ForceHttpsConfig, UrlRedirectRule};
use std::collections::HashMap;

/// Unified redirect result from the three-tier engine.
#[derive(Debug)]
pub struct RedirectResult {
    pub target_url: String,
    pub status_code: u16,
    pub source: RedirectSource,
    pub cache_control: Option<String>,
    pub response_headers: HashMap<String, String>,
}

#[derive(Debug)]
pub enum RedirectSource {
    Domain,
    Protocol,
    UrlRule,
}

/// Three-tier redirect check (priority order):
/// 1. Domain redirect (highest) — old.example.com → new.example.com
/// 2. Protocol redirect — HTTP → HTTPS (force_https)
/// 3. URL rule redirect — /old-path → /new-path
pub fn check_redirect(
    scheme: &str,
    host: &str,
    uri: &str,
    path: &str,
    query_string: Option<&str>,
    method: &str,
    domain_redirect: Option<&DomainRedirectConfig>,
    force_https: &ForceHttpsConfig,
    url_rules: &[UrlRedirectRule],
) -> Option<RedirectResult> {
    // ── Tier 1: Domain redirect ──
    if let Some(dr_config) = domain_redirect {
        if let Some(result) = domain::check_domain_redirect(host, scheme, uri, dr_config) {
            return Some(RedirectResult {
                target_url: result.target_url,
                status_code: result.status_code,
                source: RedirectSource::Domain,
                cache_control: None,
                response_headers: HashMap::new(),
            });
        }
    }

    // ── Tier 2: Protocol redirect ──
    if let Some(result) = protocol::check_protocol_redirect(scheme, host, uri, force_https) {
        return Some(RedirectResult {
            target_url: result.target_url,
            status_code: result.status_code,
            source: RedirectSource::Protocol,
            cache_control: None,
            response_headers: HashMap::new(),
        });
    }

    // ── Tier 3: URL rule redirect ──
    let req_info = url::RequestInfo {
        scheme,
        host,
        uri,
        path,
        query_string,
        method,
    };
    if let Some(result) = url::check_url_rules(&req_info, url_rules) {
        return Some(RedirectResult {
            target_url: result.target_url,
            status_code: result.status_code,
            source: RedirectSource::UrlRule,
            cache_control: result.cache_control,
            response_headers: result.response_headers,
        });
    }

    None
}
