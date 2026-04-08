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
}

impl SiteConfig {
    /// Log warnings for contradictory or suspicious configuration.
    pub fn warn_invalid(&self) {
        if self.protocol.force_https && self.protocol.force_http {
            log::warn!(
                "[Config] site '{}': force_https and force_http both true, force_https takes precedence",
                self.site_id
            );
        }
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
    pub force_https: bool,
    #[serde(default)]
    pub force_http: bool,
    #[serde(default = "default_redirect_code")]
    pub redirect_code: u16,
    #[serde(default = "default_true")]
    pub http2: bool,
    #[serde(default)]
    pub websocket: bool,
    #[serde(default)]
    pub sse: bool,
    #[serde(default)]
    pub grpc: GrpcConfig,
    #[serde(default)]
    pub https_exclude_paths: Vec<String>,
    #[serde(default)]
    pub https_port: Option<u16>,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            force_https: false,
            force_http: false,
            redirect_code: default_redirect_code(),
            http2: true,
            websocket: false,
            sse: false,
            grpc: GrpcConfig::default(),
            https_exclude_paths: Vec::new(),
            https_port: None,
        }
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
