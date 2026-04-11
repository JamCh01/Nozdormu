use crate::node_config::*;
use cdn_common::{CompressionConfig, ImageOptimizationConfig};
use serde::{Deserialize, Serialize};

/// Cluster-shared configuration loaded from etcd `{prefix}/global/*` keys.
///
/// Each field maps to a separate etcd key (e.g., `redis` → `{prefix}/global/redis`).
/// All fields are `Option` to support partial configuration — missing sections
/// fall through to environment variables or defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub redis: Option<RedisConfig>,
    pub security: Option<SecurityConfig>,
    pub balancer: Option<BalancerConfig>,
    pub proxy: Option<ProxyTimeoutConfig>,
    pub cache: Option<CacheOssConfig>,
    pub ssl: Option<SslAcmeConfig>,
    pub logging: Option<LoggingGlobalConfig>,
    pub compression: Option<CompressionConfig>,
    pub image_optimization: Option<ImageOptimizationConfig>,
}

/// Subset of logging config that is cluster-shared.
///
/// `level` stays env-only because the logger is initialized before etcd is available.
/// When `backend` is set, it takes priority over the legacy `push_to_redis` flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingGlobalConfig {
    /// DEPRECATED: use `backend` instead. Kept for backward compatibility.
    #[serde(default = "default_push_to_redis")]
    pub push_to_redis: bool,
    /// DEPRECATED: use `backend.max_len` instead. Kept for backward compatibility.
    #[serde(default = "default_stream_max_len")]
    pub stream_max_len: u64,
    /// Log backend configuration. When set, overrides `push_to_redis`.
    #[serde(default)]
    pub backend: Option<cdn_log::LogBackendConfig>,
}

fn default_push_to_redis() -> bool {
    true
}

fn default_stream_max_len() -> u64 {
    100_000
}

impl Default for LoggingGlobalConfig {
    fn default() -> Self {
        Self {
            push_to_redis: default_push_to_redis(),
            stream_max_len: default_stream_max_len(),
            backend: None,
        }
    }
}
