use cdn_common::WebhookConfig;
use chrono::Utc;
use dashmap::DashMap;
use ring::hmac;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::logging::metrics::WEBHOOK_DELIVERY_TOTAL;

// ── Event types ──

/// Webhook event variants. Serialized with `event_type` tag for JSON payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum WebhookEvent {
    CertRenewalSuccess {
        domain: String,
        provider: String,
        expires_at: i64,
        node_id: String,
    },
    CertRenewalFailure {
        domain: String,
        error: String,
        node_id: String,
    },
    HealthStatusChange {
        site_id: String,
        origin_id: String,
        healthy: bool,
        consecutive_count: u32,
        node_id: String,
    },
    CachePurgeCompleted {
        task_id: String,
        site_id: String,
        success: bool,
        keys_deleted: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        duration_secs: f64,
    },
    Test {
        message: String,
    },
}

impl WebhookEvent {
    /// Return the event type label for metrics.
    pub fn event_type_label(&self) -> &'static str {
        match self {
            Self::CertRenewalSuccess { .. } => "cert_renewal_success",
            Self::CertRenewalFailure { .. } => "cert_renewal_failure",
            Self::HealthStatusChange { .. } => "health_status_change",
            Self::CachePurgeCompleted { .. } => "cache_purge_completed",
            Self::Test { .. } => "test",
        }
    }
}

// ── Payload wrapper ──

#[derive(Debug, Serialize)]
struct WebhookPayload {
    /// ISO 8601 timestamp.
    timestamp: String,
    /// Unique delivery ID for deduplication.
    delivery_id: String,
    #[serde(flatten)]
    event: WebhookEvent,
}

// ── Delivery tracker ──

#[derive(Debug, Clone, Serialize)]
pub struct WebhookDeliveryStatus {
    pub delivery_id: String,
    pub event_type: String,
    pub url: String,
    pub status: DeliveryState,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivered,
    Failed,
}

pub struct WebhookDeliveryTracker {
    deliveries: DashMap<String, WebhookDeliveryStatus>,
}

impl WebhookDeliveryTracker {
    pub fn new() -> Self {
        Self {
            deliveries: DashMap::new(),
        }
    }

    pub fn insert(&self, status: WebhookDeliveryStatus) {
        // Auto-evict entries older than 1 hour
        let cutoff = Utc::now().timestamp() - 3600;
        self.deliveries.retain(|_, v| v.created_at > cutoff);
        self.deliveries.insert(status.delivery_id.clone(), status);
    }

    pub fn update_delivered(&self, delivery_id: &str, status_code: u16) {
        if let Some(mut entry) = self.deliveries.get_mut(delivery_id) {
            entry.status = DeliveryState::Delivered;
            entry.last_status_code = Some(status_code);
            entry.completed_at = Some(Utc::now().timestamp());
        }
    }

    pub fn update_failed(
        &self,
        delivery_id: &str,
        attempts: u32,
        last_status_code: Option<u16>,
        last_error: Option<String>,
    ) {
        if let Some(mut entry) = self.deliveries.get_mut(delivery_id) {
            entry.status = DeliveryState::Failed;
            entry.attempts = attempts;
            entry.last_status_code = last_status_code;
            entry.last_error = last_error;
            entry.completed_at = Some(Utc::now().timestamp());
        }
    }

    pub fn update_attempt(
        &self,
        delivery_id: &str,
        attempts: u32,
        status_code: Option<u16>,
        error: Option<String>,
    ) {
        if let Some(mut entry) = self.deliveries.get_mut(delivery_id) {
            entry.attempts = attempts;
            entry.last_status_code = status_code;
            entry.last_error = error;
        }
    }

    pub fn list(&self) -> Vec<WebhookDeliveryStatus> {
        let mut items: Vec<_> = self.deliveries.iter().map(|r| r.value().clone()).collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }
}

// ── HMAC signature ──

fn compute_hmac_hex(secret: &str, body: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let tag = hmac::sign(&key, body.as_bytes());
    let bytes = tag.as_ref();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

// ── Per-site dispatch ──

/// Fire-and-forget: dispatch a webhook event using the site's webhook config.
/// Does nothing if webhook is disabled or has no URLs.
/// Spawns background tasks for each URL — never blocks the caller.
pub fn dispatch(
    config: &WebhookConfig,
    event: WebhookEvent,
    tracker: &Arc<WebhookDeliveryTracker>,
) {
    if !config.enabled || config.urls.is_empty() {
        return;
    }

    let event_type = event.event_type_label().to_string();
    for url in &config.urls {
        let delivery_id = uuid::Uuid::new_v4().to_string();
        let payload = WebhookPayload {
            timestamp: Utc::now().to_rfc3339(),
            delivery_id: delivery_id.clone(),
            event: event.clone(),
        };
        let url: String = url.clone();
        let secret = config.secret.clone();
        let max_retries = config.max_retries;
        let timeout_secs = config.timeout_secs;
        let tracker = Arc::clone(tracker);
        let event_type = event_type.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(timeout_secs))
                .connect_timeout(Duration::from_secs(5))
                .user_agent("Nozdormu-Webhook/1.0")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            deliver_webhook(
                &client,
                &url,
                &payload,
                secret.as_deref(),
                max_retries,
                &tracker,
                &event_type,
            )
            .await;
        });
    }
}

// ── Delivery with retry ──

async fn deliver_webhook(
    client: &reqwest::Client,
    url: &str,
    payload: &WebhookPayload,
    secret: Option<&str>,
    max_retries: u32,
    tracker: &WebhookDeliveryTracker,
    event_type: &str,
) {
    let body = match serde_json::to_string(payload) {
        Ok(b) => b,
        Err(e) => {
            log::error!("[Webhook] failed to serialize payload: {}", e);
            return;
        }
    };

    // Register in tracker
    tracker.insert(WebhookDeliveryStatus {
        delivery_id: payload.delivery_id.clone(),
        event_type: event_type.to_string(),
        url: url.to_string(),
        status: DeliveryState::Pending,
        attempts: 0,
        last_status_code: None,
        last_error: None,
        created_at: Utc::now().timestamp(),
        completed_at: None,
    });

    for attempt in 0..=max_retries {
        if attempt > 0 {
            // Exponential backoff: 1s, 2s, 4s, 8s, capped at 16s
            let delay = Duration::from_secs(1u64 << (attempt - 1).min(4));
            tokio::time::sleep(delay).await;
        }

        let mut req = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Webhook-Delivery", &payload.delivery_id);

        // HMAC-SHA256 signature if secret configured
        if let Some(secret) = secret {
            let sig = compute_hmac_hex(secret, &body);
            req = req.header("X-Webhook-Signature", format!("sha256={}", sig));
        }

        match req.body(body.clone()).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracker.update_delivered(&payload.delivery_id, resp.status().as_u16());
                WEBHOOK_DELIVERY_TOTAL
                    .with_label_values(&[event_type, "success"])
                    .inc();
                log::debug!(
                    "[Webhook] delivered {} to {} (attempt {})",
                    event_type,
                    url,
                    attempt + 1
                );
                return;
            }
            Ok(resp) => {
                let code = resp.status().as_u16();
                log::debug!(
                    "[Webhook] non-2xx response {} from {} (attempt {})",
                    code,
                    url,
                    attempt + 1
                );
                tracker.update_attempt(&payload.delivery_id, attempt + 1, Some(code), None);
            }
            Err(e) => {
                log::debug!(
                    "[Webhook] request error to {} (attempt {}): {}",
                    url,
                    attempt + 1,
                    e
                );
                tracker.update_attempt(
                    &payload.delivery_id,
                    attempt + 1,
                    None,
                    Some(e.to_string()),
                );
            }
        }
    }

    // All retries exhausted
    let last = tracker
        .deliveries
        .get(&payload.delivery_id)
        .map(|e| (e.last_status_code, e.last_error.clone()));
    let (last_code, last_err) = last.unwrap_or((None, None));
    tracker.update_failed(&payload.delivery_id, max_retries + 1, last_code, last_err);
    WEBHOOK_DELIVERY_TOTAL
        .with_label_values(&[event_type, "failure"])
        .inc();
    log::warn!(
        "[Webhook] delivery failed after {} attempts: {} to {}",
        max_retries + 1,
        event_type,
        url
    );
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_event_serde_roundtrip() {
        let events = vec![
            WebhookEvent::CertRenewalSuccess {
                domain: "example.com".to_string(),
                provider: "letsencrypt".to_string(),
                expires_at: 1726012345,
                node_id: "node-1".to_string(),
            },
            WebhookEvent::CertRenewalFailure {
                domain: "example.com".to_string(),
                error: "rate limited".to_string(),
                node_id: "node-1".to_string(),
            },
            WebhookEvent::HealthStatusChange {
                site_id: "site-1".to_string(),
                origin_id: "origin-1".to_string(),
                healthy: false,
                consecutive_count: 3,
                node_id: "node-1".to_string(),
            },
            WebhookEvent::CachePurgeCompleted {
                task_id: "task-1".to_string(),
                site_id: "site-1".to_string(),
                success: true,
                keys_deleted: 42,
                error: None,
                duration_secs: 1.5,
            },
            WebhookEvent::Test {
                message: "hello".to_string(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let parsed: WebhookEvent = serde_json::from_str(&json).unwrap();
            // Verify event_type tag is present
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(value.get("event_type").is_some());
            // Verify roundtrip produces same JSON
            let json2 = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn test_webhook_event_type_label() {
        assert_eq!(
            WebhookEvent::CertRenewalSuccess {
                domain: String::new(),
                provider: String::new(),
                expires_at: 0,
                node_id: String::new(),
            }
            .event_type_label(),
            "cert_renewal_success"
        );
        assert_eq!(
            WebhookEvent::HealthStatusChange {
                site_id: String::new(),
                origin_id: String::new(),
                healthy: true,
                consecutive_count: 0,
                node_id: String::new(),
            }
            .event_type_label(),
            "health_status_change"
        );
        assert_eq!(
            WebhookEvent::Test {
                message: String::new(),
            }
            .event_type_label(),
            "test"
        );
    }

    #[test]
    fn test_delivery_tracker_lifecycle() {
        let tracker = WebhookDeliveryTracker::new();

        tracker.insert(WebhookDeliveryStatus {
            delivery_id: "d1".to_string(),
            event_type: "test".to_string(),
            url: "https://example.com/hook".to_string(),
            status: DeliveryState::Pending,
            attempts: 0,
            last_status_code: None,
            last_error: None,
            created_at: Utc::now().timestamp(),
            completed_at: None,
        });

        assert_eq!(tracker.list().len(), 1);
        assert_eq!(tracker.list()[0].status, DeliveryState::Pending);

        tracker.update_delivered("d1", 200);
        let items = tracker.list();
        assert_eq!(items[0].status, DeliveryState::Delivered);
        assert_eq!(items[0].last_status_code, Some(200));
        assert!(items[0].completed_at.is_some());
    }

    #[test]
    fn test_delivery_tracker_failed() {
        let tracker = WebhookDeliveryTracker::new();

        tracker.insert(WebhookDeliveryStatus {
            delivery_id: "d2".to_string(),
            event_type: "test".to_string(),
            url: "https://example.com/hook".to_string(),
            status: DeliveryState::Pending,
            attempts: 0,
            last_status_code: None,
            last_error: None,
            created_at: Utc::now().timestamp(),
            completed_at: None,
        });

        tracker.update_failed("d2", 4, Some(500), Some("server error".to_string()));
        let items = tracker.list();
        assert_eq!(items[0].status, DeliveryState::Failed);
        assert_eq!(items[0].attempts, 4);
        assert_eq!(items[0].last_status_code, Some(500));
        assert_eq!(items[0].last_error.as_deref(), Some("server error"));
    }

    #[test]
    fn test_delivery_tracker_eviction() {
        let tracker = WebhookDeliveryTracker::new();

        tracker.deliveries.insert(
            "old".to_string(),
            WebhookDeliveryStatus {
                delivery_id: "old".to_string(),
                event_type: "test".to_string(),
                url: "https://example.com".to_string(),
                status: DeliveryState::Delivered,
                attempts: 1,
                last_status_code: Some(200),
                last_error: None,
                created_at: Utc::now().timestamp() - 7200,
                completed_at: Some(Utc::now().timestamp() - 7200),
            },
        );

        assert_eq!(tracker.list().len(), 1);

        tracker.insert(WebhookDeliveryStatus {
            delivery_id: "new".to_string(),
            event_type: "test".to_string(),
            url: "https://example.com".to_string(),
            status: DeliveryState::Pending,
            attempts: 0,
            last_status_code: None,
            last_error: None,
            created_at: Utc::now().timestamp(),
            completed_at: None,
        });

        let items = tracker.list();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].delivery_id, "new");
    }

    #[test]
    fn test_dispatch_disabled_config() {
        let tracker = Arc::new(WebhookDeliveryTracker::new());
        let config = WebhookConfig::default(); // disabled
        dispatch(
            &config,
            WebhookEvent::Test {
                message: "test".to_string(),
            },
            &tracker,
        );
        // Nothing dispatched
        assert!(tracker.list().is_empty());
    }

    #[test]
    fn test_dispatch_no_urls() {
        let tracker = Arc::new(WebhookDeliveryTracker::new());
        let config = WebhookConfig {
            enabled: true,
            urls: vec![],
            ..Default::default()
        };
        dispatch(
            &config,
            WebhookEvent::Test {
                message: "test".to_string(),
            },
            &tracker,
        );
        assert!(tracker.list().is_empty());
    }

    #[test]
    fn test_hmac_signature() {
        let sig = compute_hmac_hex("my_secret", r#"{"event_type":"test"}"#);
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));

        let sig2 = compute_hmac_hex("my_secret", r#"{"event_type":"test"}"#);
        assert_eq!(sig, sig2);

        let sig3 = compute_hmac_hex("other_secret", r#"{"event_type":"test"}"#);
        assert_ne!(sig, sig3);
    }

    #[test]
    fn test_webhook_config_serde() {
        let json = r#"{
            "enabled": true,
            "urls": ["https://example.com/hook1", "https://example.com/hook2"],
            "secret": "my_secret",
            "timeout_secs": 15,
            "max_retries": 5
        }"#;
        let config: WebhookConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.urls.len(), 2);
        assert_eq!(config.secret.as_deref(), Some("my_secret"));
        assert_eq!(config.timeout_secs, 15);
        assert_eq!(config.max_retries, 5);

        let json2 = r#"{}"#;
        let config2: WebhookConfig = serde_json::from_str(json2).unwrap();
        assert!(!config2.enabled);
        assert!(config2.urls.is_empty());
        assert!(config2.secret.is_none());
        assert_eq!(config2.timeout_secs, 10);
        assert_eq!(config2.max_retries, 3);
    }

    #[test]
    fn test_delivery_tracker_list_sorted_by_created_at() {
        let tracker = WebhookDeliveryTracker::new();
        let now = Utc::now().timestamp();

        for i in 0..3 {
            tracker.deliveries.insert(
                format!("d{}", i),
                WebhookDeliveryStatus {
                    delivery_id: format!("d{}", i),
                    event_type: "test".to_string(),
                    url: "https://example.com".to_string(),
                    status: DeliveryState::Pending,
                    attempts: 0,
                    last_status_code: None,
                    last_error: None,
                    created_at: now + i as i64,
                    completed_at: None,
                },
            );
        }

        let items = tracker.list();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].delivery_id, "d2");
        assert_eq!(items[1].delivery_id, "d1");
        assert_eq!(items[2].delivery_id, "d0");
    }
}
