use crate::utils::redis_pool::RedisPool;
use serde::Serialize;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

static LOG_SENDER: OnceLock<mpsc::Sender<String>> = OnceLock::new();

/// Initialize the log queue with a Redis pool.
/// Spawns a single background consumer that batches log entries to Redis.
pub fn init_log_queue(pool: Arc<RedisPool>) {
    let (tx, rx) = mpsc::channel::<String>(8192);
    LOG_SENDER.set(tx).ok();
    tokio::spawn(log_consumer(pool, rx));
}

/// Background consumer: drains the channel in batches and writes to Redis.
async fn log_consumer(pool: Arc<RedisPool>, mut rx: mpsc::Receiver<String>) {
    loop {
        // Wait for the first entry
        let first = match rx.recv().await {
            Some(entry) => entry,
            None => break, // channel closed
        };

        // Drain up to 63 more entries without blocking
        let mut batch = Vec::with_capacity(64);
        batch.push(first);
        while batch.len() < 64 {
            match rx.try_recv() {
                Ok(entry) => batch.push(entry),
                Err(_) => break,
            }
        }

        // Write batch to Redis
        if pool.is_available() {
            for json in &batch {
                if let Err(e) = pool
                    .xadd("nozdormu:log:requests", 100_000, "data", json)
                    .await
                {
                    log::warn!("[LogQueue] Redis XADD failed: {}", e);
                    log::info!("[LogQueue:local] {}", json);
                }
            }
        } else {
            for json in &batch {
                log::debug!("[LogQueue] {}", json);
            }
        }
    }
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

/// Push a log entry to the bounded channel.
/// Drops the entry on backpressure (channel full) to prevent unbounded memory growth.
pub fn push_log_entry(entry: LogEntry) {
    let json = match serde_json::to_string(&entry) {
        Ok(j) => j,
        Err(e) => {
            log::error!("[LogQueue] serialize error: {}", e);
            return;
        }
    };

    if let Some(tx) = LOG_SENDER.get() {
        if tx.try_send(json).is_err() {
            log::debug!("[LogQueue] channel full, dropping log entry");
        }
    } else {
        log::debug!("[LogQueue] {}", json);
    }
}
