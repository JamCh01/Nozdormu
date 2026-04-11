use crate::config::LogChannelsConfig;
use crate::entry::*;
use crate::sink::LogSink;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::mpsc;

/// Internal message: (destination, json_payload).
static LOG_SENDER: OnceLock<mpsc::Sender<(String, String)>> = OnceLock::new();

/// Initialize the log queue with a sink backend.
///
/// Spawns a single background consumer that batches log entries by destination
/// and sends them to the provided sink.
pub fn init_log_queue(sink: Box<dyn LogSink>) {
    let (tx, rx) = mpsc::channel::<(String, String)>(8192);
    LOG_SENDER.set(tx).ok();
    tokio::spawn(log_consumer(sink, rx));
}

/// Background consumer: drains the channel, batches by destination, writes to sink.
async fn log_consumer(sink: Box<dyn LogSink>, mut rx: mpsc::Receiver<(String, String)>) {
    loop {
        let first = match rx.recv().await {
            Some(entry) => entry,
            None => break,
        };

        // Drain up to 127 more entries without blocking
        let mut raw = Vec::with_capacity(128);
        raw.push(first);
        while raw.len() < 128 {
            match rx.try_recv() {
                Ok(entry) => raw.push(entry),
                Err(_) => break,
            }
        }

        // Group by destination
        let mut batches: HashMap<String, Vec<String>> = HashMap::new();
        for (dest, json) in raw {
            batches.entry(dest).or_default().push(json);
        }

        // Send each batch
        for (dest, entries) in &batches {
            if let Err(e) = sink.send(dest, entries).await {
                log::warn!("[LogQueue:{}] send to {} failed: {}", sink.name(), dest, e);
                for json in entries {
                    log::info!("[LogQueue:local] {}", json);
                }
            }
        }
    }
}

/// Push log entries to all enabled channels.
///
/// Serializes the appropriate sub-struct for each enabled channel and sends
/// `(destination, json)` pairs to the bounded channel. Disabled channels
/// and channels without applicable data are skipped entirely.
pub fn push_log(channels: &LogChannelsConfig, entry: &LogEntry) {
    let tx = match LOG_SENDER.get() {
        Some(tx) => tx,
        None => return,
    };

    // Access: full log entry (always has data)
    if channels.access.enabled {
        if let Ok(json) = serde_json::to_string(entry) {
            let _ = tx.try_send((channels.access.destination.clone(), json));
        }
    }

    // Client→CDN phase (only when upstream was reached)
    if channels.client_to_cdn.enabled {
        if let Some(ms) = entry.client_to_cdn_ms {
            let log = ClientToCdnLog::from_entry(entry, ms);
            if let Ok(json) = serde_json::to_string(&log) {
                let _ = tx.try_send((channels.client_to_cdn.destination.clone(), json));
            }
        }
    }

    // CDN→Origin phase
    if channels.cdn_to_origin.enabled {
        if let Some(ms) = entry.cdn_to_origin_ms {
            let log = CdnToOriginLog::from_entry(entry, ms);
            if let Ok(json) = serde_json::to_string(&log) {
                let _ = tx.try_send((channels.cdn_to_origin.destination.clone(), json));
            }
        }
    }

    // Origin→CDN phase
    if channels.origin_to_cdn.enabled {
        if let Some(ms) = entry.origin_to_cdn_ms {
            let log = OriginToCdnLog::from_entry(entry, ms);
            if let Ok(json) = serde_json::to_string(&log) {
                let _ = tx.try_send((channels.origin_to_cdn.destination.clone(), json));
            }
        }
    }

    // CDN→Client phase
    if channels.cdn_to_client.enabled {
        if let Some(ms) = entry.cdn_to_client_ms {
            let log = CdnToClientLog::from_entry(entry, ms);
            if let Ok(json) = serde_json::to_string(&log) {
                let _ = tx.try_send((channels.cdn_to_client.destination.clone(), json));
            }
        }
    }

    // WAF events
    if channels.waf.enabled {
        let log = WafLog::from_entry(entry);
        if let Ok(json) = serde_json::to_string(&log) {
            let _ = tx.try_send((channels.waf.destination.clone(), json));
        }
    }

    // CC events
    if channels.cc.enabled {
        let log = CcLog::from_entry(entry);
        if let Ok(json) = serde_json::to_string(&log) {
            let _ = tx.try_send((channels.cc.destination.clone(), json));
        }
    }

    // Cache events
    if channels.cache.enabled {
        let log = CacheLog::from_entry(entry);
        if let Ok(json) = serde_json::to_string(&log) {
            let _ = tx.try_send((channels.cache.destination.clone(), json));
        }
    }
}

/// Legacy single-entry push (backward compat). Sends to access channel only.
pub fn push_log_entry(entry: LogEntry) {
    let tx = match LOG_SENDER.get() {
        Some(tx) => tx,
        None => {
            log::debug!("[LogQueue] not initialized");
            return;
        }
    };
    if let Ok(json) = serde_json::to_string(&entry) {
        if tx
            .try_send(("nozdormu-logs.access".to_string(), json))
            .is_err()
        {
            log::debug!("[LogQueue] channel full, dropping log entry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn sample_entry() -> LogEntry {
        LogEntry {
            timestamp: "2026-04-11T00:00:00Z".to_string(),
            request_id: "test-1".to_string(),
            method: "GET".to_string(),
            host: "example.com".to_string(),
            path: "/".to_string(),
            query_string: None,
            scheme: "http".to_string(),
            protocol: "Http".to_string(),
            client_ip: Some("127.0.0.1".parse::<IpAddr>().unwrap()),
            country_code: None,
            asn: None,
            status: 200,
            response_size: 0,
            duration_ms: 1.0,
            site_id: "test".to_string(),
            cache_status: "NONE".to_string(),
            cache_key: None,
            origin_id: None,
            origin_host: None,
            waf_blocked: false,
            waf_reason: None,
            cc_blocked: false,
            cc_reason: None,
            range_request: false,
            packaging_request: false,
            auth_validated: false,
            body_rejected: false,
            early_data: false,
            node_id: "test-node".to_string(),
            client_to_cdn_ms: None,
            cdn_to_origin_ms: None,
            origin_to_cdn_ms: None,
            cdn_to_client_ms: None,
        }
    }

    #[test]
    fn test_push_log_entry_no_sender() {
        let entry = sample_entry();
        push_log_entry(entry); // should not panic
    }

    #[test]
    fn test_push_log_no_sender() {
        let channels = LogChannelsConfig::default();
        let entry = sample_entry();
        push_log(&channels, &entry); // should not panic
    }
}
