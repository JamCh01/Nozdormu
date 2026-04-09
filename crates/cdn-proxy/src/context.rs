use cdn_common::{CompressionAlgorithm, OriginConfig, SiteConfig};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Lightweight request ID generator.
/// Uses a global atomic counter combined with a process-start timestamp
/// to produce unique, compact IDs without syscalls per request.
fn generate_request_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static START_TS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ts = *START_TS.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    });
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}", ts, count)
}

/// Per-request context carried through all ProxyHttp callbacks.
/// Replaces OpenResty's ngx.ctx (which was lost after ngx.exec).
pub struct ProxyCtx {
    // === Set in request_filter (access phase) ===
    pub client_ip: Option<IpAddr>,
    pub site_config: Option<Arc<SiteConfig>>,
    pub site_id: String,
    pub request_id: String,
    pub geo_info: Option<GeoInfo>,

    // === WAF result ===
    pub waf_blocked: bool,
    pub waf_reason: Option<String>,

    // === CC result ===
    pub cc_blocked: bool,
    pub cc_reason: Option<String>,

    // === Set in request_filter (content phase) ===
    pub protocol_type: ProtocolType,
    pub cache_status: CacheStatus,
    pub cache_key: Option<String>,
    pub cache_ttl: Option<u64>,
    pub resolved_ips: HashMap<String, IpAddr>,

    // === Cached request info (avoids re-extraction in response_filter/logging) ===
    pub host: String,
    pub uri: String,
    pub scheme: String,

    // === Set in upstream_peer (balancer phase) ===
    pub selected_origin: Option<OriginConfig>,
    pub balancer_tried: u32,

    // === Set in response_body_filter ===
    pub response_body: Option<Vec<Vec<u8>>>,
    pub response_body_size: usize,

    // === Compression state ===
    pub compressor: Option<crate::compression::Encoder>,
    pub compression_algorithm: Option<CompressionAlgorithm>,

    // === Image optimization state ===
    pub image_params: Option<cdn_image::ImageParams>,
    pub image_output_format: Option<cdn_common::ImageFormat>,
    pub image_auto_negotiated: bool,
    pub image_buffer: Vec<u8>,
    pub image_buffering: bool,

    // === Range request state ===
    pub range_request: Option<crate::range::RangeSpec>,
    pub range_passthrough: bool,
    pub range_served_from_cache: bool,
    pub total_content_length: Option<u64>,
    pub range_body_buffer: Vec<u8>,
}

impl Default for ProxyCtx {
    fn default() -> Self {
        Self {
            client_ip: None,
            site_config: None,
            site_id: String::new(),
            request_id: generate_request_id(),
            geo_info: None,
            waf_blocked: false,
            waf_reason: None,
            cc_blocked: false,
            cc_reason: None,
            protocol_type: ProtocolType::Http,
            cache_status: CacheStatus::None,
            cache_key: None,
            cache_ttl: None,
            resolved_ips: HashMap::new(),
            host: String::new(),
            uri: String::new(),
            scheme: String::new(),
            selected_origin: None,
            balancer_tried: 0,
            response_body: None,
            response_body_size: 0,
            compressor: None,
            compression_algorithm: None,
            image_params: None,
            image_output_format: None,
            image_auto_negotiated: false,
            image_buffer: Vec::new(),
            image_buffering: false,
            range_request: None,
            range_passthrough: false,
            range_served_from_cache: false,
            total_content_length: None,
            range_body_buffer: Vec::new(),
        }
    }
}

/// Protocol type detected from request headers.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolType {
    Http,
    WebSocket,
    Sse,
    Grpc(GrpcVariant),
}

#[derive(Debug, Clone, PartialEq)]
pub enum GrpcVariant {
    Native,
    Web,
    WebText,
}

/// Cache status for the current request.
#[derive(Debug, Clone, PartialEq)]
pub enum CacheStatus {
    Hit,
    Miss,
    Bypass,
    Expired,
    None,
}

impl CacheStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CacheStatus::Hit => "HIT",
            CacheStatus::Miss => "MISS",
            CacheStatus::Bypass => "BYPASS",
            CacheStatus::Expired => "EXPIRED",
            CacheStatus::None => "NONE",
        }
    }
}

/// GeoIP information cached per-request.
#[derive(Debug, Clone, Default)]
pub struct GeoInfo {
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub continent_code: Option<String>,
    pub subdivision_code: Option<String>,
    pub subdivision_name: Option<String>,
    pub city_name: Option<String>,
    pub asn: Option<u32>,
    pub asn_org: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}
