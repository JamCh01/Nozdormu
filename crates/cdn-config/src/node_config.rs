use serde::{Deserialize, Serialize};
use std::collections::HashSet;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_or_none(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ============================================================
// Top-level node configuration
// ============================================================

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub node: NodeInfo,
    pub redis: RedisConfig,
    pub etcd: EtcdConfig,
    pub cache_oss: CacheOssConfig,
    pub security: SecurityConfig,
    pub balancer: BalancerConfig,
    pub proxy: ProxyTimeoutConfig,
    pub ssl: SslAcmeConfig,
    pub log: LogConfig,
    pub paths: PathsConfig,
}

impl NodeConfig {
    pub fn from_env() -> Self {
        Self {
            node: NodeInfo::from_env(),
            redis: RedisConfig::from_env(),
            etcd: EtcdConfig::from_env(),
            cache_oss: CacheOssConfig::from_env(),
            security: SecurityConfig::from_env(),
            balancer: BalancerConfig::from_env(),
            proxy: ProxyTimeoutConfig::from_env(),
            ssl: SslAcmeConfig::from_env(),
            log: LogConfig::from_env(),
            paths: PathsConfig::from_env(),
        }
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.node.id.is_empty() {
            errors.push("CDN_NODE_ID is required".to_string());
        }
        if self.etcd.endpoints.is_empty() {
            errors.push("CDN_ETCD_ENDPOINTS is required".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn is_development(&self) -> bool {
        self.node.env == "development"
    }

    pub fn print_summary(&self) {
        log::info!("=== Nozdormu CDN Configuration ===");
        log::info!("  Node ID:     {}", self.node.id);
        log::info!("  Labels:      {:?}", self.node.labels);
        log::info!("  Environment: {}", self.node.env);
        log::info!("  Redis mode:  {}", self.redis.mode);
        log::info!("  etcd:        {}", self.etcd.endpoints.join(", "));
        log::info!("  Log level:   {}", self.log.level);
    }
}

// ============================================================
// Node identity
// ============================================================

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: String,
    pub labels: HashSet<String>,
    pub env: String,
}

impl NodeInfo {
    fn from_env() -> Self {
        let labels_str = env_or("CDN_NODE_LABELS", "");
        let labels = labels_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Self {
            id: env_or("CDN_NODE_ID", "dev-node-01"),
            labels,
            env: env_or("CDN_ENV", "development"),
        }
    }
}

// ============================================================
// Redis
// ============================================================

#[derive(Debug, Clone)]
pub struct RedisConfig {
    pub mode: String,
    pub password: Option<String>,
    pub db: u8,
    pub sentinel: RedisSentinelConfig,
    pub standalone: RedisStandaloneConfig,
    pub connect_timeout_ms: u64,
    pub send_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub pool_size: usize,
}

#[derive(Debug, Clone)]
pub struct RedisSentinelConfig {
    pub master_name: String,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RedisStandaloneConfig {
    pub host: String,
    pub port: u16,
}

impl RedisConfig {
    fn from_env() -> Self {
        let sentinels_str = env_or("CDN_REDIS_SENTINELS", "127.0.0.1:26379");
        let sentinel_nodes: Vec<String> = sentinels_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Self {
            mode: env_or("CDN_REDIS_MODE", "sentinel"),
            password: env_or_none("CDN_REDIS_PASSWORD"),
            db: std::env::var("CDN_REDIS_DB")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            sentinel: RedisSentinelConfig {
                master_name: env_or("CDN_REDIS_SENTINEL_MASTER", "mymaster"),
                nodes: sentinel_nodes,
            },
            standalone: RedisStandaloneConfig {
                host: env_or("CDN_REDIS_HOST", "127.0.0.1"),
                port: env_u16("CDN_REDIS_PORT", 6379),
            },
            connect_timeout_ms: env_u64("CDN_REDIS_CONNECT_TIMEOUT", 5000),
            send_timeout_ms: env_u64("CDN_REDIS_SEND_TIMEOUT", 5000),
            read_timeout_ms: env_u64("CDN_REDIS_READ_TIMEOUT", 5000),
            pool_size: env_usize("CDN_REDIS_POOL_SIZE", 100),
        }
    }
}

// ============================================================
// etcd
// ============================================================

#[derive(Debug, Clone)]
pub struct EtcdConfig {
    pub endpoints: Vec<String>,
    pub prefix: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub connect_timeout_ms: u64,
}

impl EtcdConfig {
    fn from_env() -> Self {
        let endpoints_str = env_or("CDN_ETCD_ENDPOINTS", "http://127.0.0.1:2379");
        let endpoints: Vec<String> = endpoints_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Self {
            endpoints,
            prefix: env_or("CDN_ETCD_PREFIX", "/nozdormu"),
            username: env_or_none("CDN_ETCD_USERNAME"),
            password: env_or_none("CDN_ETCD_PASSWORD"),
            connect_timeout_ms: env_u64("CDN_ETCD_CONNECT_TIMEOUT", 5000),
        }
    }
}

// ============================================================
// Cache / OSS
// ============================================================

#[derive(Debug, Clone)]
pub struct CacheOssConfig {
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub region: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub use_ssl: bool,
    pub path_style: bool,
    pub default_ttl: u64,
    pub max_size: u64,
}

impl CacheOssConfig {
    fn from_env() -> Self {
        Self {
            endpoint: env_or_none("CDN_OSS_ENDPOINT"),
            bucket: env_or_none("CDN_OSS_BUCKET"),
            region: env_or("CDN_OSS_REGION", "us-east-1"),
            access_key_id: env_or_none("CDN_OSS_ACCESS_KEY_ID"),
            secret_access_key: env_or_none("CDN_OSS_SECRET_ACCESS_KEY"),
            use_ssl: env_bool("CDN_OSS_USE_SSL", true),
            path_style: env_bool("CDN_OSS_PATH_STYLE", false),
            default_ttl: env_u64("CDN_CACHE_DEFAULT_TTL", 3600),
            max_size: env_u64("CDN_CACHE_MAX_SIZE", 104_857_600),
        }
    }
}

// ============================================================
// Security
// ============================================================

#[derive(Debug, Clone)]
pub struct SecurityConfig {
    pub waf_default_mode: String,
    pub cc_default_rate: u64,
    pub cc_default_window: u64,
    pub cc_default_block_duration: u64,
    pub cc_challenge_secret: String,
}

impl SecurityConfig {
    fn from_env() -> Self {
        Self {
            waf_default_mode: env_or("CDN_WAF_MODE", "block"),
            cc_default_rate: env_u64("CDN_CC_DEFAULT_RATE", 100),
            cc_default_window: env_u64("CDN_CC_DEFAULT_WINDOW", 60),
            cc_default_block_duration: env_u64("CDN_CC_BLOCK_DURATION", 600),
            cc_challenge_secret: env_or(
                "CDN_CC_CHALLENGE_SECRET",
                "cdn_default_cc_secret_change_me",
            ),
        }
    }
}

// ============================================================
// Balancer
// ============================================================

#[derive(Debug, Clone)]
pub struct BalancerConfig {
    pub default_algorithm: String,
    pub default_retries: u32,
    pub dns_nameservers: Vec<String>,
    pub health_check_interval: u64,
    pub health_check_timeout: u64,
    pub healthy_threshold: u32,
    pub unhealthy_threshold: u32,
}

impl BalancerConfig {
    fn from_env() -> Self {
        let ns_str = env_or("CDN_DNS_NAMESERVERS", "8.8.8.8,8.8.4.4");
        let nameservers: Vec<String> = ns_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Self {
            default_algorithm: env_or("CDN_LB_ALGORITHM", "round_robin"),
            default_retries: std::env::var("CDN_LB_RETRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            dns_nameservers: nameservers,
            health_check_interval: env_u64("CDN_HEALTH_CHECK_INTERVAL", 10),
            health_check_timeout: env_u64("CDN_HEALTH_CHECK_TIMEOUT", 5),
            healthy_threshold: std::env::var("CDN_HEALTHY_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            unhealthy_threshold: std::env::var("CDN_UNHEALTHY_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
        }
    }
}

// ============================================================
// Proxy timeouts
// ============================================================

#[derive(Debug, Clone)]
pub struct ProxyTimeoutConfig {
    pub connect_timeout: u64,
    pub send_timeout: u64,
    pub read_timeout: u64,
    pub websocket_timeout: u64,
    pub sse_timeout: u64,
    pub grpc_timeout: u64,
}

impl ProxyTimeoutConfig {
    fn from_env() -> Self {
        Self {
            connect_timeout: env_u64("CDN_PROXY_CONNECT_TIMEOUT", 10),
            send_timeout: env_u64("CDN_PROXY_SEND_TIMEOUT", 60),
            read_timeout: env_u64("CDN_PROXY_READ_TIMEOUT", 60),
            websocket_timeout: env_u64("CDN_WEBSOCKET_TIMEOUT", 3600),
            sse_timeout: env_u64("CDN_SSE_TIMEOUT", 86400),
            grpc_timeout: env_u64("CDN_GRPC_TIMEOUT", 300),
        }
    }
}

// ============================================================
// SSL / ACME
// ============================================================

#[derive(Debug, Clone)]
pub struct SslAcmeConfig {
    pub acme_environment: String,
    pub acme_email: Option<String>,
    pub renewal_days: u64,
    pub acme_providers: Vec<String>,
    pub eab_credentials: EabCredentials,
}

#[derive(Debug, Clone, Default)]
pub struct EabCredentials {
    pub zerossl_kid: Option<String>,
    pub zerossl_hmac_key: Option<String>,
    pub google_kid: Option<String>,
    pub google_hmac_key: Option<String>,
}

impl SslAcmeConfig {
    fn from_env() -> Self {
        let providers_str = env_or("CDN_ACME_PROVIDERS", "letsencrypt,zerossl,buypass,google");
        let providers: Vec<String> = providers_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Self {
            acme_environment: env_or("CDN_ACME_ENV", "production"),
            acme_email: env_or_none("CDN_ACME_EMAIL"),
            renewal_days: env_u64("CDN_CERT_RENEWAL_DAYS", 30),
            acme_providers: providers,
            eab_credentials: EabCredentials {
                zerossl_kid: env_or_none("CDN_ZEROSSL_EAB_KID"),
                zerossl_hmac_key: env_or_none("CDN_ZEROSSL_EAB_HMAC_KEY"),
                google_kid: env_or_none("CDN_GOOGLE_EAB_KID"),
                google_hmac_key: env_or_none("CDN_GOOGLE_EAB_HMAC_KEY"),
            },
        }
    }
}

// ============================================================
// Logging
// ============================================================

#[derive(Debug, Clone)]
pub struct LogConfig {
    pub level: String,
    pub push_to_redis: bool,
    pub stream_max_len: u64,
}

impl LogConfig {
    fn from_env() -> Self {
        Self {
            level: env_or("CDN_LOG_LEVEL", "info"),
            push_to_redis: env_bool("CDN_LOG_PUSH_REDIS", true),
            stream_max_len: env_u64("CDN_LOG_STREAM_MAX_LEN", 100_000),
        }
    }
}

// ============================================================
// Paths
// ============================================================

#[derive(Debug, Clone)]
pub struct PathsConfig {
    pub certs: String,
    pub geoip: String,
    pub logs: String,
}

impl PathsConfig {
    fn from_env() -> Self {
        Self {
            certs: env_or("CDN_CERT_PATH", "/etc/nozdormu/certs"),
            geoip: env_or("CDN_GEOIP_PATH", "/etc/nozdormu/geoip"),
            logs: env_or("CDN_LOG_PATH", "/var/log/nozdormu"),
        }
    }
}
