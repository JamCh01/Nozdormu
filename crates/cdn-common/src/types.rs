use ipnet::IpNet;
use serde::{Deserialize, Serialize};

// ============================================================
// Site Configuration (top-level)
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteConfig {
    pub site_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Site listen port (used for routing; the global listener must cover this port)
    #[serde(default = "default_port")]
    pub port: u16,
    pub domains: Vec<String>,
    #[serde(default)]
    pub target_labels: Vec<String>,
    pub origins: Vec<OriginConfig>,
    #[serde(default)]
    pub load_balancer: LoadBalancerConfig,
    #[serde(default)]
    pub protocol: ProtocolConfig,
    #[serde(default)]
    pub ssl: SslSiteConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub waf: WafConfig,
    #[serde(default)]
    pub cc: CcConfig,
    #[serde(default)]
    pub headers: HeadersConfig,
    #[serde(default)]
    pub domain_redirect: Option<DomainRedirectConfig>,
    #[serde(default)]
    pub url_redirect_rules: Vec<UrlRedirectRule>,
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
    #[serde(default)]
    pub compression: CompressionConfig,
    #[serde(default)]
    pub image_optimization: ImageOptimizationConfig,
    #[serde(default)]
    pub range: RangeConfig,
    #[serde(default)]
    pub streaming: StreamingConfig,
}

impl SiteConfig {
    /// Log warnings for contradictory or suspicious configuration.
    pub fn warn_invalid(&self) {
        if self.domains.is_empty() && self.enabled {
            log::warn!("[Config] site '{}': enabled but has no domains", self.site_id);
        }
        if self.origins.is_empty() && self.enabled {
            log::warn!("[Config] site '{}': enabled but has no origins", self.site_id);
        }
    }
}

// ============================================================
// Origin
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginConfig {
    pub id: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default)]
    pub protocol: OriginProtocol,
    #[serde(default)]
    pub backup: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub sni: Option<String>,
    #[serde(default)]
    pub verify_ssl: bool,
    #[serde(default)]
    pub target_labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OriginProtocol {
    #[default]
    Http,
    Https,
}

// ============================================================
// Load Balancer
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancerConfig {
    #[serde(default)]
    pub algorithm: LbAlgorithm,
    #[serde(default = "default_retries")]
    pub retries: u32,
    #[serde(default)]
    pub health_check: HealthCheckSiteConfig,
}

impl Default for LoadBalancerConfig {
    fn default() -> Self {
        Self {
            algorithm: LbAlgorithm::default(),
            retries: default_retries(),
            health_check: HealthCheckSiteConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LbAlgorithm {
    #[default]
    RoundRobin,
    IpHash,
    Random,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckSiteConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub r#type: HealthCheckType,
    #[serde(default = "default_health_path")]
    pub path: String,
    #[serde(default = "default_health_interval")]
    pub interval: u64,
    #[serde(default = "default_health_timeout")]
    pub timeout: u64,
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,
    /// Acceptable HTTP status codes. None = accept 200-299.
    #[serde(default)]
    pub expected_codes: Option<Vec<u16>>,
    /// Override Host header sent in HTTP health check probes.
    #[serde(default)]
    pub host_header: Option<String>,
}

impl Default for HealthCheckSiteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            r#type: HealthCheckType::default(),
            path: default_health_path(),
            interval: default_health_interval(),
            timeout: default_health_timeout(),
            healthy_threshold: default_healthy_threshold(),
            unhealthy_threshold: default_unhealthy_threshold(),
            expected_codes: None,
            host_header: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HealthCheckType {
    #[default]
    Http,
    Tcp,
}

// ============================================================
// Protocol
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolConfig {
    #[serde(default)]
    pub force_https: ForceHttpsConfig,
    #[serde(default = "default_true")]
    pub http2: bool,
    #[serde(default)]
    pub websocket: WebSocketConfig,
    #[serde(default)]
    pub sse: SseConfig,
    #[serde(default)]
    pub grpc: GrpcConfig,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            force_https: ForceHttpsConfig::default(),
            http2: true,
            websocket: WebSocketConfig::default(),
            sse: SseConfig::default(),
            grpc: GrpcConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForceHttpsConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default = "default_redirect_code")]
    pub redirect_code: u16,
    #[serde(default)]
    pub https_port: Option<u16>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
}

impl Default for ForceHttpsConfig {
    fn default() -> Self {
        Self {
            enable: false,
            redirect_code: default_redirect_code(),
            https_port: None,
            exclude_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    #[serde(default)]
    pub enable: bool,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseConfig {
    #[serde(default)]
    pub enable: bool,
}

impl Default for SseConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GrpcConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: GrpcMode,
    #[serde(default)]
    pub services: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GrpcMode {
    #[default]
    Layer7,
    Layer4,
}

// ============================================================
// SSL (site-level)
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslSiteConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub r#type: SslType,
    #[serde(default)]
    pub acme_email: Option<String>,
}

impl Default for SslSiteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            r#type: SslType::default(),
            acme_email: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SslType {
    #[default]
    Acme,
    Custom,
}

// ============================================================
// Cache
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_cache_ttl")]
    pub default_ttl: u64,
    #[serde(default = "default_cache_max_size")]
    pub max_size: u64,
    #[serde(default)]
    pub rules: Vec<CacheRule>,
    #[serde(default)]
    pub sort_query_string: bool,
    #[serde(default)]
    pub vary_headers: Vec<String>,
    #[serde(default)]
    pub cache_authorized: bool,
    #[serde(default)]
    pub cache_cookies: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_ttl: default_cache_ttl(),
            max_size: default_cache_max_size(),
            rules: Vec::new(),
            sort_query_string: false,
            vary_headers: Vec::new(),
            cache_authorized: false,
            cache_cookies: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRule {
    pub r#type: CacheRuleType,
    pub r#match: serde_json::Value,
    #[serde(default)]
    pub ttl: u64,
    #[serde(default = "default_ttl_unit")]
    pub ttl_unit: String,
    #[serde(default)]
    pub regex_options: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CacheRuleType {
    Path,
    Extension,
    Mimetype,
    Regex,
}

// ============================================================
// WAF
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WafConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: WafMode,
    #[serde(default)]
    pub rules: WafRules,
    #[serde(default)]
    pub body_inspection: BodyInspectionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WafMode {
    #[default]
    Block,
    Log,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WafRules {
    #[serde(default)]
    pub ip_whitelist: Vec<IpNet>,
    #[serde(default)]
    pub ip_blacklist: Vec<IpNet>,
    #[serde(default)]
    pub asn_blacklist: Vec<u32>,
    #[serde(default)]
    pub country_whitelist: Vec<String>,
    #[serde(default)]
    pub country_blacklist: Vec<String>,
    #[serde(default)]
    pub region_blacklist: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    pub continent_blacklist: Vec<String>,
}

// ── Body Inspection ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyInspectionConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Maximum request body size in bytes. 0 = unlimited. Default 25 MB.
    #[serde(default = "default_max_body_size")]
    pub max_body_size: u64,
    /// Allowed MIME types (magic-bytes verified). Empty = allow all.
    /// Supports wildcards: "image/*", "application/pdf"
    #[serde(default)]
    pub allowed_content_types: Vec<String>,
    /// Blocked MIME types (magic-bytes verified). Checked after allowed list.
    #[serde(default)]
    pub blocked_content_types: Vec<String>,
    /// HTTP methods to inspect. Default: ["POST", "PUT", "PATCH"]
    #[serde(default = "default_inspect_methods")]
    pub inspect_methods: Vec<String>,
}

fn default_max_body_size() -> u64 {
    26_214_400 // 25 MB
}

fn default_inspect_methods() -> Vec<String> {
    vec!["POST".into(), "PUT".into(), "PATCH".into()]
}

impl Default for BodyInspectionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_body_size: default_max_body_size(),
            allowed_content_types: Vec::new(),
            blocked_content_types: Vec::new(),
            inspect_methods: default_inspect_methods(),
        }
    }
}

// ============================================================
// CC
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CcConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_cc_rate")]
    pub default_rate: u64,
    #[serde(default = "default_cc_window")]
    pub default_window: u64,
    #[serde(default = "default_cc_block_duration")]
    pub default_block_duration: u64,
    #[serde(default)]
    pub default_action: CcAction,
    #[serde(default)]
    pub rules: Vec<CcRule>,
}

impl Default for CcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_rate: default_cc_rate(),
            default_window: default_cc_window(),
            default_block_duration: default_cc_block_duration(),
            default_action: CcAction::default(),
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CcRule {
    #[serde(default = "default_cc_path")]
    pub path: String,
    pub rate: u64,
    #[serde(default = "default_cc_window")]
    pub window: u64,
    #[serde(default = "default_cc_block_duration")]
    pub block_duration: u64,
    #[serde(default)]
    pub action: CcAction,
    #[serde(default)]
    pub key_type: CcKeyType,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CcAction {
    #[default]
    Block,
    Challenge,
    Log,
    Delay,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CcKeyType {
    Ip,
    #[default]
    IpUrl,
    IpPath,
}

// ============================================================
// Headers
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeadersConfig {
    #[serde(default)]
    pub request: Vec<HeaderRule>,
    #[serde(default)]
    pub response: Vec<HeaderRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderRule {
    pub action: HeaderAction,
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeaderAction {
    Set,
    Add,
    Remove,
    Append,
}

// ============================================================
// Domain Redirect
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainRedirectConfig {
    #[serde(default)]
    pub enabled: bool,
    pub target_domain: String,
    #[serde(default)]
    pub source_domains: Vec<String>,
    #[serde(default = "default_redirect_code")]
    pub status_code: u16,
}

// ============================================================
// URL Redirect Rules
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UrlRedirectRule {
    pub r#type: UrlRuleType,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_domain: Option<String>,
    pub target: String,
    #[serde(default = "default_redirect_code")]
    pub status_code: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub preserve_query_string: bool,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub match_query_string: bool,
    #[serde(default)]
    pub regex_options: Option<String>,
    #[serde(default)]
    pub cache_control: Option<String>,
    #[serde(default)]
    pub response_headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UrlRuleType {
    Exact,
    Prefix,
    Regex,
    Domain,
}

// ============================================================
// Timeouts
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutsConfig {
    #[serde(default = "default_connect_timeout")]
    pub connect: u64,
    #[serde(default = "default_send_timeout")]
    pub send: u64,
    #[serde(default = "default_read_timeout")]
    pub read: u64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            connect: default_connect_timeout(),
            send: default_send_timeout(),
            read: default_read_timeout(),
        }
    }
}

// ============================================================
// Default value functions
// ============================================================

fn default_true() -> bool { true }
fn default_port() -> u16 { 80 }
fn default_weight() -> u32 { 10 }
fn default_retries() -> u32 { 2 }
fn default_redirect_code() -> u16 { 301 }
fn default_health_path() -> String { "/health".to_string() }
fn default_health_interval() -> u64 { 10 }
fn default_health_timeout() -> u64 { 5 }
fn default_healthy_threshold() -> u32 { 2 }
fn default_unhealthy_threshold() -> u32 { 3 }
fn default_cache_ttl() -> u64 { 3600 }
fn default_cache_max_size() -> u64 { 104_857_600 }
fn default_ttl_unit() -> String { "seconds".to_string() }
fn default_cc_rate() -> u64 { 100 }
fn default_cc_window() -> u64 { 60 }
fn default_cc_block_duration() -> u64 { 600 }
fn default_cc_path() -> String { "/".to_string() }
fn default_connect_timeout() -> u64 { 10 }
fn default_send_timeout() -> u64 { 60 }
fn default_read_timeout() -> u64 { 60 }

// ============================================================
// Compression
// ============================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressionAlgorithm {
    Gzip,
    Brotli,
    Zstd,
}

impl CompressionAlgorithm {
    /// HTTP `Content-Encoding` token for this algorithm.
    pub fn encoding_token(&self) -> &'static str {
        match self {
            CompressionAlgorithm::Gzip => "gzip",
            CompressionAlgorithm::Brotli => "br",
            CompressionAlgorithm::Zstd => "zstd",
        }
    }

    /// Parse from `Accept-Encoding` token.
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_lowercase().as_str() {
            "gzip" => Some(CompressionAlgorithm::Gzip),
            "br" => Some(CompressionAlgorithm::Brotli),
            "zstd" => Some(CompressionAlgorithm::Zstd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_compression_algorithms")]
    pub algorithms: Vec<CompressionAlgorithm>,
    #[serde(default = "default_compression_level")]
    pub level: u32,
    #[serde(default = "default_compression_min_size")]
    pub min_size: u64,
    #[serde(default = "default_compressible_types")]
    pub compressible_types: Vec<String>,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            algorithms: default_compression_algorithms(),
            level: default_compression_level(),
            min_size: default_compression_min_size(),
            compressible_types: default_compressible_types(),
        }
    }
}

fn default_compression_algorithms() -> Vec<CompressionAlgorithm> {
    vec![
        CompressionAlgorithm::Zstd,
        CompressionAlgorithm::Brotli,
        CompressionAlgorithm::Gzip,
    ]
}

fn default_compression_level() -> u32 { 6 }
fn default_compression_min_size() -> u64 { 256 }

fn default_compressible_types() -> Vec<String> {
    vec![
        "text/*".to_string(),
        "application/json".to_string(),
        "application/javascript".to_string(),
        "application/xml".to_string(),
        "application/xhtml+xml".to_string(),
        "application/rss+xml".to_string(),
        "application/atom+xml".to_string(),
        "application/wasm".to_string(),
        "image/svg+xml".to_string(),
    ]
}

// ============================================================
// Image Optimization Configuration
// ============================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Avif,
    #[serde(alias = "webp")]
    WebP,
    Jpeg,
    Png,
}

impl ImageFormat {
    /// MIME content type for this format.
    pub fn content_type(&self) -> &'static str {
        match self {
            ImageFormat::Avif => "image/avif",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Png => "image/png",
        }
    }

    /// Token used in Accept header matching.
    pub fn accept_token(&self) -> &'static str {
        match self {
            ImageFormat::Avif => "image/avif",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Png => "image/png",
        }
    }

    /// Parse from a string token (query param value).
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_lowercase().as_str() {
            "avif" => Some(ImageFormat::Avif),
            "webp" => Some(ImageFormat::WebP),
            "jpeg" | "jpg" => Some(ImageFormat::Jpeg),
            "png" => Some(ImageFormat::Png),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResizeFit {
    /// Scale to fit within target dimensions, preserving aspect ratio.
    #[default]
    Contain,
    /// Scale to cover target dimensions, crop center.
    Cover,
    /// Stretch to exact target dimensions (ignores aspect ratio).
    Fill,
    /// Like Contain but never enlarges.
    Inside,
    /// Like Cover but never enlarges.
    Outside,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOptimizationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_image_formats")]
    pub formats: Vec<ImageFormat>,
    #[serde(default = "default_image_quality")]
    pub default_quality: u32,
    #[serde(default = "default_image_max_width")]
    pub max_width: u32,
    #[serde(default = "default_image_max_height")]
    pub max_height: u32,
    #[serde(default = "default_image_max_input_size")]
    pub max_input_size: u64,
    #[serde(default = "default_image_optimizable_types")]
    pub optimizable_types: Vec<String>,
}

impl Default for ImageOptimizationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            formats: default_image_formats(),
            default_quality: default_image_quality(),
            max_width: default_image_max_width(),
            max_height: default_image_max_height(),
            max_input_size: default_image_max_input_size(),
            optimizable_types: default_image_optimizable_types(),
        }
    }
}

fn default_image_formats() -> Vec<ImageFormat> {
    vec![ImageFormat::Avif, ImageFormat::WebP]
}

fn default_image_quality() -> u32 { 80 }
fn default_image_max_width() -> u32 { 4096 }
fn default_image_max_height() -> u32 { 4096 }
fn default_image_max_input_size() -> u64 { 50 * 1024 * 1024 } // 50 MB

fn default_image_optimizable_types() -> Vec<String> {
    vec![
        "image/jpeg".to_string(),
        "image/png".to_string(),
        "image/gif".to_string(),
        "image/bmp".to_string(),
        "image/tiff".to_string(),
    ]
}

// ============================================================
// Range Request / Chunked Origin-Pull
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeConfig {
    /// Enable Range request handling (client resume support)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Chunk size in bytes for future chunked origin-pull (Phase 2)
    #[serde(default = "default_range_chunk_size")]
    pub chunk_size: u64,
}

impl Default for RangeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            chunk_size: default_range_chunk_size(),
        }
    }
}

fn default_range_chunk_size() -> u64 {
    4 * 1024 * 1024 // 4 MB
}

// ============================================================
// Streaming Configuration (Auth + Dynamic Packaging + Prefetch)
// ============================================================

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct StreamingConfig {
    #[serde(default)]
    pub auth: StreamingAuthConfig,
    #[serde(default)]
    pub dynamic_packaging: DynamicPackagingConfig,
    #[serde(default)]
    pub prefetch: PrefetchConfig,
}

impl std::fmt::Debug for StreamingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingConfig")
            .field("auth", &self.auth)
            .field("dynamic_packaging", &self.dynamic_packaging)
            .field("prefetch", &self.prefetch)
            .finish()
    }
}

// --- Edge Auth / URL Signing ---

#[derive(Clone, Serialize, Deserialize)]
pub struct StreamingAuthConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auth_type: AuthType,
    /// HMAC-SHA256 secret key for URL signing
    #[serde(default)]
    pub auth_key: String,
    /// Token expiry window in seconds (default 1800 = 30 min)
    #[serde(default = "default_auth_expire")]
    pub expire_time: u64,
}

impl Default for StreamingAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auth_type: AuthType::default(),
            auth_key: String::new(),
            expire_time: default_auth_expire(),
        }
    }
}

impl std::fmt::Debug for StreamingAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingAuthConfig")
            .field("enabled", &self.enabled)
            .field("auth_type", &self.auth_type)
            .field("auth_key", &"[REDACTED]")
            .field("expire_time", &self.expire_time)
            .finish()
    }
}

fn default_auth_expire() -> u64 {
    1800
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    #[default]
    A,
    B,
    C,
}

// --- Dynamic Packaging (MP4 → HLS) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicPackagingConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Target segment duration in seconds
    #[serde(default = "default_segment_duration")]
    pub segment_duration: f64,
    /// Maximum input MP4 file size in bytes (default 2 GB)
    #[serde(default = "default_max_mp4_size")]
    pub max_mp4_size: u64,
}

impl Default for DynamicPackagingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            segment_duration: default_segment_duration(),
            max_mp4_size: default_max_mp4_size(),
        }
    }
}

fn default_segment_duration() -> f64 {
    6.0
}

fn default_max_mp4_size() -> u64 {
    2 * 1024 * 1024 * 1024 // 2 GB
}

// --- Smart Prefetching ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefetchConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Number of segments ahead to prefetch
    #[serde(default = "default_prefetch_count")]
    pub prefetch_count: u32,
    /// Maximum concurrent prefetch requests per site
    #[serde(default = "default_prefetch_concurrency")]
    pub concurrency_limit: u32,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prefetch_count: default_prefetch_count(),
            concurrency_limit: default_prefetch_concurrency(),
        }
    }
}

fn default_prefetch_count() -> u32 {
    3
}

fn default_prefetch_concurrency() -> u32 {
    4
}
