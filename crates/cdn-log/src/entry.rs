use serde::Serialize;
use std::net::IpAddr;

// ============================================================
// Full log entry (used for access channel + internal transport)
// ============================================================

/// Complete log entry for a finished request. Contains all fields.
/// Used as the access channel payload and as the source for all sub-structs.
#[derive(Debug, Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub request_id: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub query_string: Option<String>,
    pub scheme: String,
    pub protocol: String,
    pub client_ip: Option<IpAddr>,
    pub country_code: Option<String>,
    pub asn: Option<u32>,
    pub status: u16,
    pub response_size: u64,
    pub duration_ms: f64,
    pub site_id: String,
    pub cache_status: String,
    pub cache_key: Option<String>,
    pub origin_id: Option<String>,
    pub origin_host: Option<String>,
    pub waf_blocked: bool,
    pub waf_reason: Option<String>,
    pub cc_blocked: bool,
    pub cc_reason: Option<String>,
    pub range_request: bool,
    pub packaging_request: bool,
    pub auth_validated: bool,
    pub body_rejected: bool,
    pub early_data: bool,
    pub node_id: String,
    pub client_to_cdn_ms: Option<f64>,
    pub cdn_to_origin_ms: Option<f64>,
    pub origin_to_cdn_ms: Option<f64>,
    pub cdn_to_client_ms: Option<f64>,
}

// ============================================================
// Phase sub-structs (4 channels)
// ============================================================

/// Client→CDN phase: request received → upstream connect start.
#[derive(Debug, Serialize)]
pub struct ClientToCdnLog {
    pub timestamp: String,
    pub request_id: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub scheme: String,
    pub client_ip: Option<IpAddr>,
    pub country_code: Option<String>,
    pub asn: Option<u32>,
    pub site_id: String,
    pub protocol: String,
    pub early_data: bool,
    pub client_to_cdn_ms: f64,
    pub node_id: String,
}

impl ClientToCdnLog {
    pub fn from_entry(e: &LogEntry, ms: f64) -> Self {
        Self {
            timestamp: e.timestamp.clone(),
            request_id: e.request_id.clone(),
            method: e.method.clone(),
            host: e.host.clone(),
            path: e.path.clone(),
            scheme: e.scheme.clone(),
            client_ip: e.client_ip,
            country_code: e.country_code.clone(),
            asn: e.asn,
            site_id: e.site_id.clone(),
            protocol: e.protocol.clone(),
            early_data: e.early_data,
            client_to_cdn_ms: ms,
            node_id: e.node_id.clone(),
        }
    }
}

/// CDN→Origin phase: upstream connect start → connection established.
#[derive(Debug, Serialize)]
pub struct CdnToOriginLog {
    pub timestamp: String,
    pub request_id: String,
    pub site_id: String,
    pub origin_id: Option<String>,
    pub origin_host: Option<String>,
    pub cdn_to_origin_ms: f64,
    pub node_id: String,
}

impl CdnToOriginLog {
    pub fn from_entry(e: &LogEntry, ms: f64) -> Self {
        Self {
            timestamp: e.timestamp.clone(),
            request_id: e.request_id.clone(),
            site_id: e.site_id.clone(),
            origin_id: e.origin_id.clone(),
            origin_host: e.origin_host.clone(),
            cdn_to_origin_ms: ms,
            node_id: e.node_id.clone(),
        }
    }
}

/// Origin→CDN phase: request sent → response headers received.
#[derive(Debug, Serialize)]
pub struct OriginToCdnLog {
    pub timestamp: String,
    pub request_id: String,
    pub site_id: String,
    pub origin_id: Option<String>,
    pub status: u16,
    pub response_size: u64,
    pub origin_to_cdn_ms: f64,
    pub node_id: String,
}

impl OriginToCdnLog {
    pub fn from_entry(e: &LogEntry, ms: f64) -> Self {
        Self {
            timestamp: e.timestamp.clone(),
            request_id: e.request_id.clone(),
            site_id: e.site_id.clone(),
            origin_id: e.origin_id.clone(),
            status: e.status,
            response_size: e.response_size,
            origin_to_cdn_ms: ms,
            node_id: e.node_id.clone(),
        }
    }
}

/// CDN→Client phase: response headers received → response fully sent.
#[derive(Debug, Serialize)]
pub struct CdnToClientLog {
    pub timestamp: String,
    pub request_id: String,
    pub site_id: String,
    pub status: u16,
    pub response_size: u64,
    pub cache_status: String,
    pub cdn_to_client_ms: f64,
    pub duration_ms: f64,
    pub node_id: String,
}

impl CdnToClientLog {
    pub fn from_entry(e: &LogEntry, ms: f64) -> Self {
        Self {
            timestamp: e.timestamp.clone(),
            request_id: e.request_id.clone(),
            site_id: e.site_id.clone(),
            status: e.status,
            response_size: e.response_size,
            cache_status: e.cache_status.clone(),
            cdn_to_client_ms: ms,
            duration_ms: e.duration_ms,
            node_id: e.node_id.clone(),
        }
    }
}

// ============================================================
// Event sub-structs (3 channels)
// ============================================================

/// WAF check event.
#[derive(Debug, Serialize)]
pub struct WafLog {
    pub timestamp: String,
    pub request_id: String,
    pub site_id: String,
    pub client_ip: Option<IpAddr>,
    pub country_code: Option<String>,
    pub asn: Option<u32>,
    pub method: String,
    pub host: String,
    pub path: String,
    pub waf_blocked: bool,
    pub waf_reason: Option<String>,
    pub node_id: String,
}

impl WafLog {
    pub fn from_entry(e: &LogEntry) -> Self {
        Self {
            timestamp: e.timestamp.clone(),
            request_id: e.request_id.clone(),
            site_id: e.site_id.clone(),
            client_ip: e.client_ip,
            country_code: e.country_code.clone(),
            asn: e.asn,
            method: e.method.clone(),
            host: e.host.clone(),
            path: e.path.clone(),
            waf_blocked: e.waf_blocked,
            waf_reason: e.waf_reason.clone(),
            node_id: e.node_id.clone(),
        }
    }
}

/// CC rate-limit event.
#[derive(Debug, Serialize)]
pub struct CcLog {
    pub timestamp: String,
    pub request_id: String,
    pub site_id: String,
    pub client_ip: Option<IpAddr>,
    pub method: String,
    pub host: String,
    pub path: String,
    pub cc_blocked: bool,
    pub cc_reason: Option<String>,
    pub node_id: String,
}

impl CcLog {
    pub fn from_entry(e: &LogEntry) -> Self {
        Self {
            timestamp: e.timestamp.clone(),
            request_id: e.request_id.clone(),
            site_id: e.site_id.clone(),
            client_ip: e.client_ip,
            method: e.method.clone(),
            host: e.host.clone(),
            path: e.path.clone(),
            cc_blocked: e.cc_blocked,
            cc_reason: e.cc_reason.clone(),
            node_id: e.node_id.clone(),
        }
    }
}

/// Cache hit/miss/bypass event.
#[derive(Debug, Serialize)]
pub struct CacheLog {
    pub timestamp: String,
    pub request_id: String,
    pub site_id: String,
    pub cache_status: String,
    pub cache_key: Option<String>,
    pub host: String,
    pub path: String,
    pub node_id: String,
}

impl CacheLog {
    pub fn from_entry(e: &LogEntry) -> Self {
        Self {
            timestamp: e.timestamp.clone(),
            request_id: e.request_id.clone(),
            site_id: e.site_id.clone(),
            cache_status: e.cache_status.clone(),
            cache_key: e.cache_key.clone(),
            host: e.host.clone(),
            path: e.path.clone(),
            node_id: e.node_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> LogEntry {
        LogEntry {
            timestamp: "2026-04-11T00:00:00Z".to_string(),
            request_id: "abc-1".to_string(),
            method: "GET".to_string(),
            host: "example.com".to_string(),
            path: "/test".to_string(),
            query_string: None,
            scheme: "https".to_string(),
            protocol: "Http".to_string(),
            client_ip: Some("1.2.3.4".parse().unwrap()),
            country_code: Some("US".to_string()),
            asn: Some(13335),
            status: 200,
            response_size: 1024,
            duration_ms: 50.0,
            site_id: "test-site".to_string(),
            cache_status: "MISS".to_string(),
            cache_key: Some("abc123".to_string()),
            origin_id: Some("origin-1".to_string()),
            origin_host: Some("backend.example.com".to_string()),
            waf_blocked: false,
            waf_reason: None,
            cc_blocked: false,
            cc_reason: None,
            range_request: false,
            packaging_request: false,
            auth_validated: false,
            body_rejected: false,
            early_data: false,
            node_id: "node-1".to_string(),
            client_to_cdn_ms: Some(5.0),
            cdn_to_origin_ms: Some(10.0),
            origin_to_cdn_ms: Some(20.0),
            cdn_to_client_ms: Some(15.0),
        }
    }

    #[test]
    fn test_access_log_all_fields() {
        let entry = sample_entry();
        let json = serde_json::to_string(&entry).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["client_to_cdn_ms"], 5.0);
        assert_eq!(v["cdn_to_origin_ms"], 10.0);
        assert_eq!(v["status"], 200);
    }

    #[test]
    fn test_client_to_cdn_log() {
        let entry = sample_entry();
        let log = ClientToCdnLog::from_entry(&entry, 5.0);
        let json = serde_json::to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["client_to_cdn_ms"], 5.0);
        assert_eq!(v["method"], "GET");
        assert_eq!(v["host"], "example.com");
        assert!(v.get("status").is_none()); // not in this sub-struct
    }

    #[test]
    fn test_cdn_to_origin_log() {
        let entry = sample_entry();
        let log = CdnToOriginLog::from_entry(&entry, 10.0);
        let json = serde_json::to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["cdn_to_origin_ms"], 10.0);
        assert_eq!(v["origin_id"], "origin-1");
        assert!(v.get("method").is_none());
    }

    #[test]
    fn test_origin_to_cdn_log() {
        let entry = sample_entry();
        let log = OriginToCdnLog::from_entry(&entry, 20.0);
        let json = serde_json::to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["origin_to_cdn_ms"], 20.0);
        assert_eq!(v["status"], 200);
    }

    #[test]
    fn test_cdn_to_client_log() {
        let entry = sample_entry();
        let log = CdnToClientLog::from_entry(&entry, 15.0);
        let json = serde_json::to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["cdn_to_client_ms"], 15.0);
        assert_eq!(v["cache_status"], "MISS");
        assert_eq!(v["duration_ms"], 50.0);
    }

    #[test]
    fn test_waf_log() {
        let entry = sample_entry();
        let log = WafLog::from_entry(&entry);
        let json = serde_json::to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["waf_blocked"], false);
        assert_eq!(v["client_ip"], "1.2.3.4");
    }

    #[test]
    fn test_cc_log() {
        let entry = sample_entry();
        let log = CcLog::from_entry(&entry);
        let json = serde_json::to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["cc_blocked"], false);
        assert_eq!(v["path"], "/test");
    }

    #[test]
    fn test_cache_log() {
        let entry = sample_entry();
        let log = CacheLog::from_entry(&entry);
        let json = serde_json::to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["cache_status"], "MISS");
        assert_eq!(v["cache_key"], "abc123");
    }
}
