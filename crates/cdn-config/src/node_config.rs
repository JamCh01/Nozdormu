use cdn_common::{CompressionConfig, ImageOptimizationConfig};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::str::FromStr;

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

/// Check if an environment variable is explicitly set (non-empty).
fn env_is_set(key: &str) -> bool {
    std::env::var(key).ok().filter(|s| !s.is_empty()).is_some()
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
    pub compression: CompressionConfig,
    pub image_optimization: ImageOptimizationConfig,
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
            compression: compression_from_env(),
            image_optimization: image_optimization_from_env(),
        }
    }

    /// Build NodeConfig from etcd global config with env var overrides.
    ///
    /// Bootstrap configs (node, etcd, paths) are always env-only.
    /// Cluster-shared configs use etcd as base with env override.
    /// Priority: env var (if explicitly set) > etcd value > default.
    pub fn from_etcd_and_env(global: &crate::global_config::GlobalConfig) -> Self {
        Self {
            // Always env-only (bootstrap)
            node: NodeInfo::from_env(),
            etcd: EtcdConfig::from_env(),
            paths: PathsConfig::from_env(),
            // Hybrid: etcd base + env override
            redis: RedisConfig::from_etcd_with_env_override(global.redis.as_ref()),
            security: SecurityConfig::from_etcd_with_env_override(global.security.as_ref()),
            balancer: BalancerConfig::from_etcd_with_env_override(global.balancer.as_ref()),
            proxy: ProxyTimeoutConfig::from_etcd_with_env_override(global.proxy.as_ref()),
            cache_oss: CacheOssConfig::from_etcd_with_env_override(global.cache.as_ref()),
            ssl: SslAcmeConfig::from_etcd_with_env_override(global.ssl.as_ref()),
            log: LogConfig::from_etcd_with_env_override(global.logging.as_ref()),
            compression: compression_from_etcd_with_env_override(
                global.compression.as_ref(),
            ),
            image_optimization: image_optimization_from_etcd_with_env_override(
                global.image_optimization.as_ref(),
            ),
        }
    }

    /// Build NodeConfig from CLI bootstrap values + etcd global config.
    ///
    /// Bootstrap configs (node, etcd, paths) come from CLI args.
    /// Cluster-shared configs (including security secrets) use etcd as base
    /// with env override.
    pub fn from_etcd_and_cli(
        global: &crate::global_config::GlobalConfig,
        bootstrap: &BootstrapConfig,
    ) -> Self {
        Self {
            node: bootstrap.node.clone(),
            etcd: bootstrap.etcd.clone(),
            paths: bootstrap.paths.clone(),
            redis: RedisConfig::from_etcd_with_env_override(global.redis.as_ref()),
            security: SecurityConfig::from_etcd_with_env_override(
                global.security.as_ref(),
            ),
            balancer: BalancerConfig::from_etcd_with_env_override(global.balancer.as_ref()),
            proxy: ProxyTimeoutConfig::from_etcd_with_env_override(global.proxy.as_ref()),
            cache_oss: CacheOssConfig::from_etcd_with_env_override(global.cache.as_ref()),
            ssl: SslAcmeConfig::from_etcd_with_env_override(global.ssl.as_ref()),
            log: LogConfig::from_etcd_with_env_override(global.logging.as_ref()),
            compression: compression_from_etcd_with_env_override(
                global.compression.as_ref(),
            ),
            image_optimization: image_optimization_from_etcd_with_env_override(
                global.image_optimization.as_ref(),
            ),
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
        if !self.is_development()
            && self.security.cc_challenge_secret == "cdn_default_cc_secret_change_me"
        {
            errors.push(
                "cc_challenge_secret must be set in non-development environments (via etcd global/security)".to_string(),
            );
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

    pub fn from_cli(id: String, labels_str: String, env: String) -> Self {
        let labels = labels_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Self { id, labels, env }
    }
}

// ============================================================
// Bootstrap config (pre-etcd, env-only)
// ============================================================

/// Minimal config needed before etcd is available.
/// Used to bootstrap the etcd connection itself.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub node: NodeInfo,
    pub etcd: EtcdConfig,
    pub paths: PathsConfig,
    pub log_level: String,
}

impl BootstrapConfig {
    pub fn from_env() -> Self {
        Self {
            node: NodeInfo::from_env(),
            etcd: EtcdConfig::from_env(),
            paths: PathsConfig::from_env(),
            log_level: env_or("CDN_LOG_LEVEL", "info"),
        }
    }

    pub fn from_cli(
        node: NodeInfo,
        etcd: EtcdConfig,
        paths: PathsConfig,
        log_level: String,
    ) -> Self {
        Self {
            node,
            etcd,
            paths,
            log_level,
        }
    }
}

// ============================================================
// Redis
// ============================================================

#[derive(Clone, Serialize, Deserialize)]
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

impl std::fmt::Debug for RedisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisConfig")
            .field("mode", &self.mode)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("db", &self.db)
            .field("sentinel", &self.sentinel)
            .field("standalone", &self.standalone)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("send_timeout_ms", &self.send_timeout_ms)
            .field("read_timeout_ms", &self.read_timeout_ms)
            .field("pool_size", &self.pool_size)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisSentinelConfig {
    pub master_name: String,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisStandaloneConfig {
    pub host: String,
    pub port: u16,
}

impl Default for RedisSentinelConfig {
    fn default() -> Self {
        Self {
            master_name: "mymaster".to_string(),
            nodes: vec!["127.0.0.1:26379".to_string()],
        }
    }
}

impl Default for RedisStandaloneConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 6379,
        }
    }
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            mode: "sentinel".to_string(),
            password: None,
            db: 0,
            sentinel: RedisSentinelConfig::default(),
            standalone: RedisStandaloneConfig::default(),
            connect_timeout_ms: 5000,
            send_timeout_ms: 5000,
            read_timeout_ms: 5000,
            pool_size: 100,
        }
    }
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

    /// Build from etcd base with env var overrides.
    /// Priority: env var (if explicitly set) > etcd value > default.
    pub(crate) fn from_etcd_with_env_override(base: Option<&Self>) -> Self {
        let d = base.cloned().unwrap_or_default();
        Self {
            mode: if env_is_set("CDN_REDIS_MODE") {
                env_or("CDN_REDIS_MODE", "sentinel")
            } else {
                d.mode
            },
            password: if env_is_set("CDN_REDIS_PASSWORD") {
                env_or_none("CDN_REDIS_PASSWORD")
            } else {
                d.password
            },
            db: if env_is_set("CDN_REDIS_DB") {
                std::env::var("CDN_REDIS_DB")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            } else {
                d.db
            },
            sentinel: RedisSentinelConfig {
                master_name: if env_is_set("CDN_REDIS_SENTINEL_MASTER") {
                    env_or("CDN_REDIS_SENTINEL_MASTER", "mymaster")
                } else {
                    d.sentinel.master_name
                },
                nodes: if env_is_set("CDN_REDIS_SENTINELS") {
                    env_or("CDN_REDIS_SENTINELS", "127.0.0.1:26379")
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else {
                    d.sentinel.nodes
                },
            },
            standalone: RedisStandaloneConfig {
                host: if env_is_set("CDN_REDIS_HOST") {
                    env_or("CDN_REDIS_HOST", "127.0.0.1")
                } else {
                    d.standalone.host
                },
                port: if env_is_set("CDN_REDIS_PORT") {
                    env_u16("CDN_REDIS_PORT", 6379)
                } else {
                    d.standalone.port
                },
            },
            connect_timeout_ms: if env_is_set("CDN_REDIS_CONNECT_TIMEOUT") {
                env_u64("CDN_REDIS_CONNECT_TIMEOUT", 5000)
            } else {
                d.connect_timeout_ms
            },
            send_timeout_ms: if env_is_set("CDN_REDIS_SEND_TIMEOUT") {
                env_u64("CDN_REDIS_SEND_TIMEOUT", 5000)
            } else {
                d.send_timeout_ms
            },
            read_timeout_ms: if env_is_set("CDN_REDIS_READ_TIMEOUT") {
                env_u64("CDN_REDIS_READ_TIMEOUT", 5000)
            } else {
                d.read_timeout_ms
            },
            pool_size: if env_is_set("CDN_REDIS_POOL_SIZE") {
                env_usize("CDN_REDIS_POOL_SIZE", 100)
            } else {
                d.pool_size
            },
        }
    }
}

// ============================================================
// etcd
// ============================================================

#[derive(Clone)]
pub struct EtcdConfig {
    pub endpoints: Vec<String>,
    pub prefix: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub connect_timeout_ms: u64,
}

impl std::fmt::Debug for EtcdConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EtcdConfig")
            .field("endpoints", &self.endpoints)
            .field("prefix", &self.prefix)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .finish()
    }
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

    pub fn from_cli(
        endpoints_str: String,
        prefix: String,
        username: Option<String>,
        password: Option<String>,
        connect_timeout_ms: u64,
    ) -> Self {
        let endpoints: Vec<String> = endpoints_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Self {
            endpoints,
            prefix,
            username,
            password,
            connect_timeout_ms,
        }
    }
}

// ============================================================
// Cache / OSS
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for CacheOssConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            bucket: None,
            region: "us-east-1".to_string(),
            access_key_id: None,
            secret_access_key: None,
            use_ssl: true,
            path_style: false,
            default_ttl: 3600,
            max_size: 104_857_600,
        }
    }
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

    pub(crate) fn from_etcd_with_env_override(base: Option<&Self>) -> Self {
        let d = base.cloned().unwrap_or_default();
        Self {
            endpoint: if env_is_set("CDN_OSS_ENDPOINT") {
                env_or_none("CDN_OSS_ENDPOINT")
            } else {
                d.endpoint
            },
            bucket: if env_is_set("CDN_OSS_BUCKET") {
                env_or_none("CDN_OSS_BUCKET")
            } else {
                d.bucket
            },
            region: if env_is_set("CDN_OSS_REGION") {
                env_or("CDN_OSS_REGION", "us-east-1")
            } else {
                d.region
            },
            // Credentials prefer env — should not be stored in etcd plaintext
            access_key_id: if env_is_set("CDN_OSS_ACCESS_KEY_ID") {
                env_or_none("CDN_OSS_ACCESS_KEY_ID")
            } else {
                d.access_key_id
            },
            secret_access_key: if env_is_set("CDN_OSS_SECRET_ACCESS_KEY") {
                env_or_none("CDN_OSS_SECRET_ACCESS_KEY")
            } else {
                d.secret_access_key
            },
            use_ssl: if env_is_set("CDN_OSS_USE_SSL") {
                env_bool("CDN_OSS_USE_SSL", true)
            } else {
                d.use_ssl
            },
            path_style: if env_is_set("CDN_OSS_PATH_STYLE") {
                env_bool("CDN_OSS_PATH_STYLE", false)
            } else {
                d.path_style
            },
            default_ttl: if env_is_set("CDN_CACHE_DEFAULT_TTL") {
                env_u64("CDN_CACHE_DEFAULT_TTL", 3600)
            } else {
                d.default_ttl
            },
            max_size: if env_is_set("CDN_CACHE_MAX_SIZE") {
                env_u64("CDN_CACHE_MAX_SIZE", 104_857_600)
            } else {
                d.max_size
            },
        }
    }
}

// ============================================================
// Security
// ============================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub waf_default_mode: String,
    pub cc_default_rate: u64,
    pub cc_default_window: u64,
    pub cc_default_block_duration: u64,
    pub cc_challenge_secret: String,
    /// Extra trusted proxy CIDRs for X-Forwarded-For parsing.
    pub trusted_proxies: Vec<IpNet>,
    /// Optional Bearer token for admin API authentication.
    #[serde(default)]
    pub admin_token: Option<String>,
}

impl std::fmt::Debug for SecurityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityConfig")
            .field("waf_default_mode", &self.waf_default_mode)
            .field("cc_default_rate", &self.cc_default_rate)
            .field("cc_default_window", &self.cc_default_window)
            .field("cc_default_block_duration", &self.cc_default_block_duration)
            .field("cc_challenge_secret", &"[REDACTED]")
            .field("trusted_proxies", &self.trusted_proxies)
            .field("admin_token", &self.admin_token.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            waf_default_mode: "block".to_string(),
            cc_default_rate: 100,
            cc_default_window: 60,
            cc_default_block_duration: 600,
            cc_challenge_secret: "cdn_default_cc_secret_change_me".to_string(),
            trusted_proxies: Vec::new(),
            admin_token: None,
        }
    }
}

impl SecurityConfig {
    fn from_env() -> Self {
        let trusted_str = env_or("CDN_TRUSTED_PROXIES", "");
        let trusted_proxies: Vec<IpNet> = trusted_str
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter_map(|s| match IpNet::from_str(s) {
                Ok(net) => Some(net),
                Err(e) => {
                    log::warn!("[Config] invalid trusted proxy CIDR '{}': {}", s, e);
                    None
                }
            })
            .collect();

        Self {
            waf_default_mode: env_or("CDN_WAF_MODE", "block"),
            cc_default_rate: env_u64("CDN_CC_DEFAULT_RATE", 100),
            cc_default_window: env_u64("CDN_CC_DEFAULT_WINDOW", 60),
            cc_default_block_duration: env_u64("CDN_CC_BLOCK_DURATION", 600),
            cc_challenge_secret: env_or(
                "CDN_CC_CHALLENGE_SECRET",
                "cdn_default_cc_secret_change_me",
            ),
            trusted_proxies,
            admin_token: env_or_none("CDN_ADMIN_TOKEN"),
        }
    }

    fn parse_trusted_proxies(s: &str) -> Vec<IpNet> {
        s.split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter_map(|s| match IpNet::from_str(s) {
                Ok(net) => Some(net),
                Err(e) => {
                    log::warn!("[Config] invalid trusted proxy CIDR '{}': {}", s, e);
                    None
                }
            })
            .collect()
    }

    pub(crate) fn from_etcd_with_env_override(base: Option<&Self>) -> Self {
        let d = base.cloned().unwrap_or_default();
        Self {
            waf_default_mode: if env_is_set("CDN_WAF_MODE") {
                env_or("CDN_WAF_MODE", "block")
            } else {
                d.waf_default_mode
            },
            cc_default_rate: if env_is_set("CDN_CC_DEFAULT_RATE") {
                env_u64("CDN_CC_DEFAULT_RATE", 100)
            } else {
                d.cc_default_rate
            },
            cc_default_window: if env_is_set("CDN_CC_DEFAULT_WINDOW") {
                env_u64("CDN_CC_DEFAULT_WINDOW", 60)
            } else {
                d.cc_default_window
            },
            cc_default_block_duration: if env_is_set("CDN_CC_BLOCK_DURATION") {
                env_u64("CDN_CC_BLOCK_DURATION", 600)
            } else {
                d.cc_default_block_duration
            },
            cc_challenge_secret: if env_is_set("CDN_CC_CHALLENGE_SECRET") {
                env_or("CDN_CC_CHALLENGE_SECRET", "cdn_default_cc_secret_change_me")
            } else {
                d.cc_challenge_secret
            },
            trusted_proxies: if env_is_set("CDN_TRUSTED_PROXIES") {
                Self::parse_trusted_proxies(&env_or("CDN_TRUSTED_PROXIES", ""))
            } else {
                d.trusted_proxies
            },
            admin_token: if env_is_set("CDN_ADMIN_TOKEN") {
                env_or_none("CDN_ADMIN_TOKEN")
            } else {
                d.admin_token
            },
        }
    }
}

// ============================================================
// Balancer
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalancerConfig {
    pub default_algorithm: String,
    pub default_retries: u32,
    pub dns_nameservers: Vec<String>,
    pub health_check_interval: u64,
    pub health_check_timeout: u64,
    pub healthy_threshold: u32,
    pub unhealthy_threshold: u32,
}

impl Default for BalancerConfig {
    fn default() -> Self {
        Self {
            default_algorithm: "round_robin".to_string(),
            default_retries: 2,
            dns_nameservers: vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
            health_check_interval: 10,
            health_check_timeout: 5,
            healthy_threshold: 2,
            unhealthy_threshold: 3,
        }
    }
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

    pub(crate) fn from_etcd_with_env_override(base: Option<&Self>) -> Self {
        let d = base.cloned().unwrap_or_default();
        Self {
            default_algorithm: if env_is_set("CDN_LB_ALGORITHM") {
                env_or("CDN_LB_ALGORITHM", "round_robin")
            } else {
                d.default_algorithm
            },
            default_retries: if env_is_set("CDN_LB_RETRIES") {
                std::env::var("CDN_LB_RETRIES")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(2)
            } else {
                d.default_retries
            },
            dns_nameservers: if env_is_set("CDN_DNS_NAMESERVERS") {
                env_or("CDN_DNS_NAMESERVERS", "8.8.8.8,8.8.4.4")
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                d.dns_nameservers
            },
            health_check_interval: if env_is_set("CDN_HEALTH_CHECK_INTERVAL") {
                env_u64("CDN_HEALTH_CHECK_INTERVAL", 10)
            } else {
                d.health_check_interval
            },
            health_check_timeout: if env_is_set("CDN_HEALTH_CHECK_TIMEOUT") {
                env_u64("CDN_HEALTH_CHECK_TIMEOUT", 5)
            } else {
                d.health_check_timeout
            },
            healthy_threshold: if env_is_set("CDN_HEALTHY_THRESHOLD") {
                std::env::var("CDN_HEALTHY_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(2)
            } else {
                d.healthy_threshold
            },
            unhealthy_threshold: if env_is_set("CDN_UNHEALTHY_THRESHOLD") {
                std::env::var("CDN_UNHEALTHY_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(3)
            } else {
                d.unhealthy_threshold
            },
        }
    }
}

// ============================================================
// Proxy timeouts
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyTimeoutConfig {
    pub connect_timeout: u64,
    pub send_timeout: u64,
    pub read_timeout: u64,
    pub websocket_timeout: u64,
    pub sse_timeout: u64,
    pub grpc_timeout: u64,
}

impl Default for ProxyTimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout: 10,
            send_timeout: 60,
            read_timeout: 60,
            websocket_timeout: 3600,
            sse_timeout: 86400,
            grpc_timeout: 300,
        }
    }
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

    pub(crate) fn from_etcd_with_env_override(base: Option<&Self>) -> Self {
        let d = base.cloned().unwrap_or_default();
        Self {
            connect_timeout: if env_is_set("CDN_PROXY_CONNECT_TIMEOUT") {
                env_u64("CDN_PROXY_CONNECT_TIMEOUT", 10)
            } else {
                d.connect_timeout
            },
            send_timeout: if env_is_set("CDN_PROXY_SEND_TIMEOUT") {
                env_u64("CDN_PROXY_SEND_TIMEOUT", 60)
            } else {
                d.send_timeout
            },
            read_timeout: if env_is_set("CDN_PROXY_READ_TIMEOUT") {
                env_u64("CDN_PROXY_READ_TIMEOUT", 60)
            } else {
                d.read_timeout
            },
            websocket_timeout: if env_is_set("CDN_WEBSOCKET_TIMEOUT") {
                env_u64("CDN_WEBSOCKET_TIMEOUT", 3600)
            } else {
                d.websocket_timeout
            },
            sse_timeout: if env_is_set("CDN_SSE_TIMEOUT") {
                env_u64("CDN_SSE_TIMEOUT", 86400)
            } else {
                d.sse_timeout
            },
            grpc_timeout: if env_is_set("CDN_GRPC_TIMEOUT") {
                env_u64("CDN_GRPC_TIMEOUT", 300)
            } else {
                d.grpc_timeout
            },
        }
    }
}

// ============================================================
// SSL / ACME
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslAcmeConfig {
    pub acme_environment: String,
    pub acme_email: Option<String>,
    pub renewal_days: u64,
    pub acme_providers: Vec<String>,
    pub eab_credentials: EabCredentials,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct EabCredentials {
    pub zerossl_kid: Option<String>,
    pub zerossl_hmac_key: Option<String>,
    pub google_kid: Option<String>,
    pub google_hmac_key: Option<String>,
}

impl std::fmt::Debug for EabCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EabCredentials")
            .field("zerossl_kid", &self.zerossl_kid.as_ref().map(|_| "[REDACTED]"))
            .field("zerossl_hmac_key", &self.zerossl_hmac_key.as_ref().map(|_| "[REDACTED]"))
            .field("google_kid", &self.google_kid.as_ref().map(|_| "[REDACTED]"))
            .field("google_hmac_key", &self.google_hmac_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Default for SslAcmeConfig {
    fn default() -> Self {
        Self {
            acme_environment: "production".to_string(),
            acme_email: None,
            renewal_days: 30,
            acme_providers: vec![
                "letsencrypt".to_string(),
                "zerossl".to_string(),
                "buypass".to_string(),
                "google".to_string(),
            ],
            eab_credentials: EabCredentials::default(),
        }
    }
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

    pub(crate) fn from_etcd_with_env_override(base: Option<&Self>) -> Self {
        let d = base.cloned().unwrap_or_default();
        Self {
            acme_environment: if env_is_set("CDN_ACME_ENV") {
                env_or("CDN_ACME_ENV", "production")
            } else {
                d.acme_environment
            },
            acme_email: if env_is_set("CDN_ACME_EMAIL") {
                env_or_none("CDN_ACME_EMAIL")
            } else {
                d.acme_email
            },
            renewal_days: if env_is_set("CDN_CERT_RENEWAL_DAYS") {
                env_u64("CDN_CERT_RENEWAL_DAYS", 30)
            } else {
                d.renewal_days
            },
            acme_providers: if env_is_set("CDN_ACME_PROVIDERS") {
                env_or("CDN_ACME_PROVIDERS", "letsencrypt,zerossl,buypass,google")
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                d.acme_providers
            },
            // EAB credentials prefer env — should not be stored in etcd plaintext
            eab_credentials: EabCredentials {
                zerossl_kid: if env_is_set("CDN_ZEROSSL_EAB_KID") {
                    env_or_none("CDN_ZEROSSL_EAB_KID")
                } else {
                    d.eab_credentials.zerossl_kid
                },
                zerossl_hmac_key: if env_is_set("CDN_ZEROSSL_EAB_HMAC_KEY") {
                    env_or_none("CDN_ZEROSSL_EAB_HMAC_KEY")
                } else {
                    d.eab_credentials.zerossl_hmac_key
                },
                google_kid: if env_is_set("CDN_GOOGLE_EAB_KID") {
                    env_or_none("CDN_GOOGLE_EAB_KID")
                } else {
                    d.eab_credentials.google_kid
                },
                google_hmac_key: if env_is_set("CDN_GOOGLE_EAB_HMAC_KEY") {
                    env_or_none("CDN_GOOGLE_EAB_HMAC_KEY")
                } else {
                    d.eab_credentials.google_hmac_key
                },
            },
        }
    }
}

// ============================================================
// Logging
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    pub level: String,
    pub push_to_redis: bool,
    pub stream_max_len: u64,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            push_to_redis: true,
            stream_max_len: 100_000,
        }
    }
}

impl LogConfig {
    fn from_env() -> Self {
        Self {
            level: env_or("CDN_LOG_LEVEL", "info"),
            push_to_redis: env_bool("CDN_LOG_PUSH_REDIS", true),
            stream_max_len: env_u64("CDN_LOG_STREAM_MAX_LEN", 100_000),
        }
    }

    /// Build from etcd base with env var overrides.
    /// `level` is always env-only (logger init happens before etcd).
    pub(crate) fn from_etcd_with_env_override(
        base: Option<&crate::global_config::LoggingGlobalConfig>,
    ) -> Self {
        let d = base.cloned().unwrap_or_default();
        Self {
            // level is always from env — logger is initialized before etcd
            level: env_or("CDN_LOG_LEVEL", "info"),
            push_to_redis: if env_is_set("CDN_LOG_PUSH_REDIS") {
                env_bool("CDN_LOG_PUSH_REDIS", true)
            } else {
                d.push_to_redis
            },
            stream_max_len: if env_is_set("CDN_LOG_STREAM_MAX_LEN") {
                env_u64("CDN_LOG_STREAM_MAX_LEN", 100_000)
            } else {
                d.stream_max_len
            },
        }
    }
}

// ============================================================
// Paths
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    pub fn from_cli(certs: String, geoip: String, logs: String) -> Self {
        Self { certs, geoip, logs }
    }
}

// ============================================================
// Compression (env helpers for CompressionConfig)
// ============================================================

fn compression_from_env() -> CompressionConfig {
    use cdn_common::CompressionAlgorithm;
    let mut config = CompressionConfig::default();
    if env_is_set("CDN_COMPRESSION_ENABLED") {
        config.enabled = env_bool("CDN_COMPRESSION_ENABLED", false);
    }
    if env_is_set("CDN_COMPRESSION_ALGORITHMS") {
        config.algorithms = env_or("CDN_COMPRESSION_ALGORITHMS", "zstd,brotli,gzip")
            .split(',')
            .filter_map(|s| CompressionAlgorithm::from_token(s.trim()))
            .collect();
    }
    if env_is_set("CDN_COMPRESSION_LEVEL") {
        config.level = env_u64("CDN_COMPRESSION_LEVEL", 6) as u32;
    }
    if env_is_set("CDN_COMPRESSION_MIN_SIZE") {
        config.min_size = env_u64("CDN_COMPRESSION_MIN_SIZE", 256);
    }
    config
}

fn compression_from_etcd_with_env_override(base: Option<&CompressionConfig>) -> CompressionConfig {
    use cdn_common::CompressionAlgorithm;
    let d = base.cloned().unwrap_or_default();
    CompressionConfig {
        enabled: if env_is_set("CDN_COMPRESSION_ENABLED") {
            env_bool("CDN_COMPRESSION_ENABLED", false)
        } else {
            d.enabled
        },
        algorithms: if env_is_set("CDN_COMPRESSION_ALGORITHMS") {
            env_or("CDN_COMPRESSION_ALGORITHMS", "zstd,brotli,gzip")
                .split(',')
                .filter_map(|s| CompressionAlgorithm::from_token(s.trim()))
                .collect()
        } else {
            d.algorithms
        },
        level: if env_is_set("CDN_COMPRESSION_LEVEL") {
            env_u64("CDN_COMPRESSION_LEVEL", 6) as u32
        } else {
            d.level
        },
        min_size: if env_is_set("CDN_COMPRESSION_MIN_SIZE") {
            env_u64("CDN_COMPRESSION_MIN_SIZE", 256)
        } else {
            d.min_size
        },
        compressible_types: d.compressible_types,
    }
}

// ============================================================
// Image Optimization
// ============================================================

fn image_optimization_from_env() -> ImageOptimizationConfig {
    let mut config = ImageOptimizationConfig::default();
    if env_is_set("CDN_IMAGE_ENABLED") {
        config.enabled = env_bool("CDN_IMAGE_ENABLED", false);
    }
    if env_is_set("CDN_IMAGE_DEFAULT_QUALITY") {
        config.default_quality = env_u64("CDN_IMAGE_DEFAULT_QUALITY", 80) as u32;
    }
    if env_is_set("CDN_IMAGE_MAX_WIDTH") {
        config.max_width = env_u64("CDN_IMAGE_MAX_WIDTH", 4096) as u32;
    }
    if env_is_set("CDN_IMAGE_MAX_HEIGHT") {
        config.max_height = env_u64("CDN_IMAGE_MAX_HEIGHT", 4096) as u32;
    }
    if env_is_set("CDN_IMAGE_MAX_INPUT_SIZE") {
        config.max_input_size = env_u64("CDN_IMAGE_MAX_INPUT_SIZE", 50 * 1024 * 1024);
    }
    config
}

fn image_optimization_from_etcd_with_env_override(
    base: Option<&ImageOptimizationConfig>,
) -> ImageOptimizationConfig {
    let d = base.cloned().unwrap_or_default();
    ImageOptimizationConfig {
        enabled: if env_is_set("CDN_IMAGE_ENABLED") {
            env_bool("CDN_IMAGE_ENABLED", false)
        } else {
            d.enabled
        },
        formats: d.formats,
        default_quality: if env_is_set("CDN_IMAGE_DEFAULT_QUALITY") {
            env_u64("CDN_IMAGE_DEFAULT_QUALITY", 80) as u32
        } else {
            d.default_quality
        },
        max_width: if env_is_set("CDN_IMAGE_MAX_WIDTH") {
            env_u64("CDN_IMAGE_MAX_WIDTH", 4096) as u32
        } else {
            d.max_width
        },
        max_height: if env_is_set("CDN_IMAGE_MAX_HEIGHT") {
            env_u64("CDN_IMAGE_MAX_HEIGHT", 4096) as u32
        } else {
            d.max_height
        },
        max_input_size: if env_is_set("CDN_IMAGE_MAX_INPUT_SIZE") {
            env_u64("CDN_IMAGE_MAX_INPUT_SIZE", 50 * 1024 * 1024)
        } else {
            d.max_input_size
        },
        optimizable_types: d.optimizable_types,
    }
}
