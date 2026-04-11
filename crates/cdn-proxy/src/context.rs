use cdn_common::{CompressionAlgorithm, OriginConfig, SiteConfig};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
    // === Timing ===
    pub start_time: std::time::Instant,
    /// Set at start of upstream_peer() — marks end of request processing phase.
    pub upstream_start: Option<std::time::Instant>,
    /// Set at start of upstream_request_filter() — marks connection established.
    pub upstream_connected: Option<std::time::Instant>,
    /// Set at start of response_filter() — marks upstream response headers received.
    pub upstream_response_received: Option<std::time::Instant>,

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

    /// Whether this request was received via TLS 1.3 early data (0-RTT)
    pub is_early_data: bool,

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

    // === Streaming state ===
    /// Auth: cleaned path after stripping auth tokens
    pub auth_cleaned_path: Option<String>,
    /// Dynamic packaging: detected HLS request type
    pub packaging_request: Option<cdn_streaming::packaging::PackagingRequest>,
    /// Packaging: buffering MP4 body for transmux
    pub packaging_buffering: bool,
    pub packaging_buffer: Vec<u8>,
    /// Prefetch: manifest type detected, trigger prefetch on end_of_stream
    pub prefetch_manifest_type: Option<ManifestType>,
    pub prefetch_body_buffer: Vec<u8>,
    pub prefetch_buffering: bool,

    // === Cache write state ===
    pub cached_response_headers: Vec<(String, String)>,
    pub cached_response_status: u16,
    pub cached_response_tags: Vec<String>,
    pub cached_response_swr: u64,

    // === Request coalescing state ===
    pub is_coalescing_leader: bool,

    // === Body inspection state ===
    pub body_inspection_enabled: bool,
    pub body_inspection_checked: bool,
    pub body_bytes_received: u64,
    pub body_max_size: u64,
    pub body_first_chunk: Vec<u8>,
    pub body_rejected: bool,
}

impl Default for ProxyCtx {
    fn default() -> Self {
        Self {
            start_time: std::time::Instant::now(),
            upstream_start: None,
            upstream_connected: None,
            upstream_response_received: None,
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
            is_early_data: false,
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
            auth_cleaned_path: None,
            packaging_request: None,
            packaging_buffering: false,
            packaging_buffer: Vec::new(),
            prefetch_manifest_type: None,
            prefetch_body_buffer: Vec::new(),
            prefetch_buffering: false,
            cached_response_headers: Vec::new(),
            cached_response_status: 0,
            cached_response_tags: Vec::new(),
            cached_response_swr: 0,
            is_coalescing_leader: false,
            body_inspection_enabled: false,
            body_inspection_checked: false,
            body_bytes_received: 0,
            body_max_size: 0,
            body_first_chunk: Vec::new(),
            body_rejected: false,
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
    Stale,
    None,
}

impl CacheStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CacheStatus::Hit => "HIT",
            CacheStatus::Miss => "MISS",
            CacheStatus::Bypass => "BYPASS",
            CacheStatus::Expired => "EXPIRED",
            CacheStatus::Stale => "STALE",
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

/// Manifest type for prefetch detection.
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestType {
    Hls,
    Dash,
}
