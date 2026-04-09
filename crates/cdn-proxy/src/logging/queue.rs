use crate::utils::redis_pool::RedisPool;
use serde::Serialize;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

static LOG_REDIS: OnceLock<Arc<RedisPool>> = OnceLock::new();

/// Initialize the log queue with a Redis pool.
/// Call once during startup.
pub fn init_log_queue(pool: Arc<RedisPool>) {
    LOG_REDIS.set(pool).ok();
}

/// Log entry for a completed request.
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
    pub node_id: String,
}

/// Push a log entry to Redis Streams (fire-and-forget).
/// Falls back to local log if Redis is unavailable or XADD fails.
pub fn push_log_entry(entry: LogEntry) {
    let json = match serde_json::to_string(&entry) {
        Ok(j) => j,
        Err(e) => {
            log::error!("[LogQueue] serialize error: {}", e);
            return;
        }
    };

    if let Some(pool) = LOG_REDIS.get() {
        if pool.is_available() {
            let pool = Arc::clone(pool);
            let json_fallback = json.clone();
            tokio::spawn(async move {
                if let Err(e) = pool.xadd(
                    "nozdormu:log:requests",
                    100_000,
                    "data",
                    &json_fallback,
                ).await {
                    log::warn!("[LogQueue] Redis XADD failed, fallback to local: {}", e);
                    log::info!("[LogQueue:local] {}", json_fallback);
                }
            });
            return;
        }
    }

    log::debug!("[LogQueue] {}", json);
}
