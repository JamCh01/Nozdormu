use cdn_cache::key::generate_cache_key;
use cdn_cache::storage::CacheStorage;
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

use crate::logging::metrics::{CACHE_PURGE_DURATION, CACHE_PURGE_KEYS_TOTAL, CACHE_PURGE_TOTAL};
use crate::utils::redis_pool::RedisPool;

// ── Request / Response types ──

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PurgeRequest {
    Url {
        site_id: String,
        host: String,
        path: String,
        #[serde(default)]
        query_string: Option<String>,
        #[serde(default)]
        sort_query_string: bool,
        #[serde(default)]
        vary_headers: Vec<(String, String)>,
    },
    Site {
        site_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PurgeTaskStatus {
    pub task_id: String,
    pub site_id: String,
    pub status: PurgeTaskState,
    pub keys_deleted: u64,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeTaskState {
    Running,
    Completed,
    Failed,
}

// ── Task tracker ──

pub struct PurgeTaskTracker {
    tasks: DashMap<String, PurgeTaskStatus>,
}

impl Default for PurgeTaskTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PurgeTaskTracker {
    pub fn new() -> Self {
        Self {
            tasks: DashMap::new(),
        }
    }

    /// Insert a new task. Auto-evicts completed tasks older than 1 hour.
    pub fn insert(&self, status: PurgeTaskStatus) {
        let now = Utc::now().timestamp();
        let evict_before = now - 3600;

        // Evict old completed/failed tasks
        self.tasks.retain(|_, v| match v.status {
            PurgeTaskState::Running => true,
            _ => v.completed_at.unwrap_or(v.started_at) > evict_before,
        });

        self.tasks.insert(status.task_id.clone(), status);
    }

    pub fn get(&self, task_id: &str) -> Option<PurgeTaskStatus> {
        self.tasks.get(task_id).map(|v| v.clone())
    }

    pub fn list(&self) -> Vec<PurgeTaskStatus> {
        self.tasks
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub fn update_completed(&self, task_id: &str, keys_deleted: u64) {
        if let Some(mut entry) = self.tasks.get_mut(task_id) {
            entry.status = PurgeTaskState::Completed;
            entry.keys_deleted = keys_deleted;
            entry.completed_at = Some(Utc::now().timestamp());
        }
    }

    pub fn update_failed(&self, task_id: &str, error: String) {
        if let Some(mut entry) = self.tasks.get_mut(task_id) {
            entry.status = PurgeTaskState::Failed;
            entry.error = Some(error);
            entry.completed_at = Some(Utc::now().timestamp());
        }
    }

    /// Check if there is an active (running) purge for the given site_id.
    pub fn has_active_for_site(&self, site_id: &str) -> bool {
        self.tasks.iter().any(|entry| {
            entry.site_id == site_id && matches!(entry.status, PurgeTaskState::Running)
        })
    }
}

// ── Validation ──

pub fn validate_site_id(site_id: &str) -> Result<(), String> {
    if site_id.is_empty() {
        return Err("site_id cannot be empty".to_string());
    }
    if site_id.len() > 64 {
        return Err("site_id too long (max 64 chars)".to_string());
    }
    if site_id.contains('/') || site_id.contains('\\') || site_id.contains("..") {
        return Err("site_id contains invalid characters".to_string());
    }
    Ok(())
}

pub fn validate_purge_url(host: &str, path: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("host cannot be empty".to_string());
    }
    if !path.starts_with('/') {
        return Err("path must start with /".to_string());
    }
    Ok(())
}

// ── Purge execution ──

/// Purge a single URL by regenerating its cache key and deleting.
/// Returns the cache key that was purged.
pub async fn purge_url(
    cache_storage: &CacheStorage,
    site_id: &str,
    host: &str,
    path: &str,
    query_string: Option<&str>,
    sort_query_string: bool,
    vary_headers: &[(String, String)],
) -> Result<String, String> {
    let start = Instant::now();

    let cache_key = generate_cache_key(
        site_id,
        host,
        path,
        query_string,
        sort_query_string,
        vary_headers,
    );

    let result = cache_storage.delete(site_id, &cache_key).await;
    let elapsed = start.elapsed().as_secs_f64();

    let result_label = if result.is_ok() { "ok" } else { "error" };
    CACHE_PURGE_TOTAL
        .with_label_values(&[site_id, "url", result_label])
        .inc();
    CACHE_PURGE_DURATION
        .with_label_values(&[site_id, "url"])
        .observe(elapsed);

    if result.is_ok() {
        CACHE_PURGE_KEYS_TOTAL
            .with_label_values(&[site_id, "url"])
            .inc_by(1);
    }

    result.map(|_| cache_key)
}

/// Execute a site-wide purge in the background.
/// 1. Redis SCAN for `nozdormu:cache:meta:{site_id}:*`
/// 2. Extract cache keys, call `cache_storage.delete_many()`
/// 3. Fallback to OSS-only listing if Redis unavailable
pub async fn purge_site_background(
    redis_pool: Arc<RedisPool>,
    cache_storage: Arc<CacheStorage>,
    site_id: String,
    task_tracker: Arc<PurgeTaskTracker>,
    task_id: String,
) {
    let start = Instant::now();

    log::info!(
        "[Purge] starting site purge: site={} task={}",
        site_id,
        task_id
    );

    let result = purge_site_inner(&redis_pool, &cache_storage, &site_id).await;
    let elapsed = start.elapsed().as_secs_f64();

    match result {
        Ok(keys_deleted) => {
            log::info!(
                "[Purge] site purge completed: site={} keys={} duration={:.2}s",
                site_id,
                keys_deleted,
                elapsed
            );
            task_tracker.update_completed(&task_id, keys_deleted);

            CACHE_PURGE_TOTAL
                .with_label_values(&[site_id.as_str(), "site", "ok"])
                .inc();
            CACHE_PURGE_KEYS_TOTAL
                .with_label_values(&[site_id.as_str(), "site"])
                .inc_by(keys_deleted);
        }
        Err(ref e) => {
            log::error!(
                "[Purge] site purge failed: site={} error={} duration={:.2}s",
                site_id,
                e,
                elapsed
            );
            task_tracker.update_failed(&task_id, e.clone());

            CACHE_PURGE_TOTAL
                .with_label_values(&[site_id.as_str(), "site", "error"])
                .inc();
        }
    }

    CACHE_PURGE_DURATION
        .with_label_values(&[site_id.as_str(), "site"])
        .observe(elapsed);
}

async fn purge_site_inner(
    redis_pool: &RedisPool,
    cache_storage: &CacheStorage,
    site_id: &str,
) -> Result<u64, String> {
    let pattern = format!("nozdormu:cache:meta:{}:*", site_id);
    let prefix = format!("nozdormu:cache:meta:{}:", site_id);

    // Try Redis SCAN first
    match redis_pool.scan_keys(&pattern, 100).await {
        Ok(redis_keys) if !redis_keys.is_empty() => {
            log::info!(
                "[Purge] Redis SCAN found {} keys for site {}",
                redis_keys.len(),
                site_id
            );

            // Extract cache_key from Redis key (strip prefix)
            let cache_keys: Vec<String> = redis_keys
                .iter()
                .filter_map(|k| k.strip_prefix(&prefix).map(|s| s.to_string()))
                .collect();

            let deleted = cache_storage.delete_many(site_id, &cache_keys).await?;
            Ok(deleted as u64)
        }
        Ok(_) => {
            // No keys found via SCAN — try OSS-only fallback
            log::info!(
                "[Purge] no Redis keys found for site {}, trying OSS listing",
                site_id
            );
            let deleted = cache_storage.delete_site_oss_only(site_id).await?;
            Ok(deleted as u64)
        }
        Err(e) => {
            // Redis unavailable — fallback to OSS-only
            log::warn!(
                "[Purge] Redis SCAN failed for site {}: {}, falling back to OSS listing",
                site_id,
                e
            );
            let deleted = cache_storage.delete_site_oss_only(site_id).await?;
            Ok(deleted as u64)
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_purge_request_url_deserialization() {
        let json = r#"{
            "type": "url",
            "site_id": "my-site",
            "host": "example.com",
            "path": "/assets/logo.png",
            "query_string": "v=2"
        }"#;
        let req: PurgeRequest = serde_json::from_str(json).unwrap();
        match req {
            PurgeRequest::Url {
                site_id,
                host,
                path,
                query_string,
                ..
            } => {
                assert_eq!(site_id, "my-site");
                assert_eq!(host, "example.com");
                assert_eq!(path, "/assets/logo.png");
                assert_eq!(query_string, Some("v=2".to_string()));
            }
            _ => panic!("expected Url variant"),
        }
    }

    #[test]
    fn test_purge_request_site_deserialization() {
        let json = r#"{"type": "site", "site_id": "my-site"}"#;
        let req: PurgeRequest = serde_json::from_str(json).unwrap();
        match req {
            PurgeRequest::Site { site_id } => assert_eq!(site_id, "my-site"),
            _ => panic!("expected Site variant"),
        }
    }

    #[test]
    fn test_purge_request_invalid_type() {
        let json = r#"{"type": "prefix", "site_id": "x"}"#;
        let result = serde_json::from_str::<PurgeRequest>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_site_id() {
        assert!(validate_site_id("my-site").is_ok());
        assert!(validate_site_id("site_123").is_ok());
        assert!(validate_site_id("").is_err());
        assert!(validate_site_id("a/b").is_err());
        assert!(validate_site_id("a\\b").is_err());
        assert!(validate_site_id("a..b").is_err());
        assert!(validate_site_id(&"x".repeat(65)).is_err());
        assert!(validate_site_id(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn test_validate_purge_url() {
        assert!(validate_purge_url("example.com", "/path").is_ok());
        assert!(validate_purge_url("", "/path").is_err());
        assert!(validate_purge_url("example.com", "path").is_err());
    }

    #[test]
    fn test_purge_task_tracker_lifecycle() {
        let tracker = PurgeTaskTracker::new();

        let status = PurgeTaskStatus {
            task_id: "task-1".to_string(),
            site_id: "site-1".to_string(),
            status: PurgeTaskState::Running,
            keys_deleted: 0,
            started_at: Utc::now().timestamp(),
            completed_at: None,
            error: None,
        };
        tracker.insert(status);

        // Can retrieve
        let got = tracker.get("task-1").unwrap();
        assert!(matches!(got.status, PurgeTaskState::Running));
        assert!(tracker.has_active_for_site("site-1"));
        assert!(!tracker.has_active_for_site("site-2"));

        // Update completed
        tracker.update_completed("task-1", 42);
        let got = tracker.get("task-1").unwrap();
        assert!(matches!(got.status, PurgeTaskState::Completed));
        assert_eq!(got.keys_deleted, 42);
        assert!(got.completed_at.is_some());
        assert!(!tracker.has_active_for_site("site-1"));

        // List
        let all = tracker.list();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_purge_task_tracker_failed() {
        let tracker = PurgeTaskTracker::new();

        let status = PurgeTaskStatus {
            task_id: "task-2".to_string(),
            site_id: "site-1".to_string(),
            status: PurgeTaskState::Running,
            keys_deleted: 0,
            started_at: Utc::now().timestamp(),
            completed_at: None,
            error: None,
        };
        tracker.insert(status);

        tracker.update_failed("task-2", "OSS error".to_string());
        let got = tracker.get("task-2").unwrap();
        assert!(matches!(got.status, PurgeTaskState::Failed));
        assert_eq!(got.error, Some("OSS error".to_string()));
    }

    #[test]
    fn test_purge_task_tracker_eviction() {
        let tracker = PurgeTaskTracker::new();

        // Insert an old completed task (started_at 2 hours ago)
        let old_status = PurgeTaskStatus {
            task_id: "old-task".to_string(),
            site_id: "site-1".to_string(),
            status: PurgeTaskState::Completed,
            keys_deleted: 10,
            started_at: Utc::now().timestamp() - 7200,
            completed_at: Some(Utc::now().timestamp() - 7200),
            error: None,
        };
        tracker.tasks.insert("old-task".to_string(), old_status);

        // Insert a new task — should trigger eviction of old one
        let new_status = PurgeTaskStatus {
            task_id: "new-task".to_string(),
            site_id: "site-2".to_string(),
            status: PurgeTaskState::Running,
            keys_deleted: 0,
            started_at: Utc::now().timestamp(),
            completed_at: None,
            error: None,
        };
        tracker.insert(new_status);

        assert!(tracker.get("old-task").is_none());
        assert!(tracker.get("new-task").is_some());
    }

    #[test]
    fn test_purge_task_tracker_get_nonexistent() {
        let tracker = PurgeTaskTracker::new();
        assert!(tracker.get("nonexistent").is_none());
    }
}
