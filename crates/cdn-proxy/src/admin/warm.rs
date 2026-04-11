use cdn_cache::storage::CacheStorage;
use cdn_common::SiteConfig;
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;

use crate::logging::metrics::{CACHE_WARM_DURATION, CACHE_WARM_TOTAL};

// ── Request / Response types ──

#[derive(Debug, Deserialize)]
pub struct WarmRequest {
    pub site_id: String,
    pub urls: Vec<WarmUrl>,
}

#[derive(Debug, Deserialize)]
pub struct WarmUrl {
    pub host: String,
    pub path: String,
    #[serde(default)]
    pub query_string: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WarmTaskStatus {
    pub task_id: String,
    pub site_id: String,
    pub status: WarmTaskState,
    pub urls_total: u32,
    pub urls_completed: u32,
    pub urls_failed: u32,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmTaskState {
    Running,
    Completed,
    Failed,
}

// ── Task tracker ──

pub struct WarmTaskTracker {
    tasks: DashMap<String, WarmTaskStatus>,
}

impl Default for WarmTaskTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl WarmTaskTracker {
    pub fn new() -> Self {
        Self {
            tasks: DashMap::new(),
        }
    }

    /// Insert a new task. Auto-evicts completed tasks older than 1 hour.
    pub fn insert(&self, status: WarmTaskStatus) {
        let now = Utc::now().timestamp();
        let evict_before = now - 3600;

        self.tasks.retain(|_, v| match v.status {
            WarmTaskState::Running => true,
            _ => v.completed_at.unwrap_or(v.started_at) > evict_before,
        });

        self.tasks.insert(status.task_id.clone(), status);
    }

    pub fn get(&self, task_id: &str) -> Option<WarmTaskStatus> {
        self.tasks.get(task_id).map(|v| v.clone())
    }

    pub fn list(&self) -> Vec<WarmTaskStatus> {
        self.tasks
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub fn update_completed(&self, task_id: &str, urls_completed: u32, urls_failed: u32) {
        if let Some(mut entry) = self.tasks.get_mut(task_id) {
            entry.status = WarmTaskState::Completed;
            entry.urls_completed = urls_completed;
            entry.urls_failed = urls_failed;
            entry.completed_at = Some(Utc::now().timestamp());
        }
    }

    pub fn update_failed(&self, task_id: &str, error: String) {
        if let Some(mut entry) = self.tasks.get_mut(task_id) {
            entry.status = WarmTaskState::Failed;
            entry.error = Some(error);
            entry.completed_at = Some(Utc::now().timestamp());
        }
    }
}

// ── Warm execution ──

/// Execute cache warming in the background.
/// Fetches URLs from origin via reqwest and writes to cache.
pub async fn warm_urls_background(
    cache_storage: Arc<CacheStorage>,
    site_config: Arc<SiteConfig>,
    site_id: String,
    urls: Vec<WarmUrl>,
    task_tracker: Arc<WarmTaskTracker>,
    task_id: String,
) {
    let start = Instant::now();

    log::info!(
        "[Warm] starting cache warm: site={} urls={} task={}",
        site_id,
        urls.len(),
        task_id
    );

    let origin = match site_config.origins.iter().find(|o| o.enabled && !o.backup) {
        Some(o) => o.clone(),
        None => {
            log::error!("[Warm] no healthy origin for site {}", site_id);
            task_tracker.update_failed(&task_id, "no healthy origin".to_string());
            CACHE_WARM_TOTAL
                .with_label_values(&[site_id.as_str(), "error"])
                .inc();
            return;
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .pool_max_idle_per_host(10)
        .build()
        .unwrap_or_default();

    let concurrency = 10usize;
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let completed = Arc::new(AtomicU32::new(0));
    let failed = Arc::new(AtomicU32::new(0));

    let mut handles = Vec::with_capacity(urls.len());

    for warm_url in urls {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let storage = Arc::clone(&cache_storage);
        let origin = origin.clone();
        let client = client.clone();
        let sid = site_id.clone();
        let site = Arc::clone(&site_config);
        let comp = Arc::clone(&completed);
        let fail = Arc::clone(&failed);

        handles.push(tokio::spawn(async move {
            let _permit = permit;

            let scheme = match origin.protocol {
                cdn_common::OriginProtocol::Https => "https",
                cdn_common::OriginProtocol::Http => "http",
            };
            let url = match &warm_url.query_string {
                Some(q) if !q.is_empty() => format!(
                    "{}://{}:{}{}?{}",
                    scheme, origin.host, origin.port, warm_url.path, q
                ),
                _ => format!(
                    "{}://{}:{}{}",
                    scheme, origin.host, origin.port, warm_url.path
                ),
            };

            let cache_key = cdn_cache::key::generate_cache_key(
                &sid,
                &warm_url.host,
                &warm_url.path,
                warm_url.query_string.as_deref(),
                site.cache.sort_query_string,
                &[],
            );

            // Skip if already cached
            if let Some((_, false)) = storage.get_with_stale(&sid, &cache_key).await {
                comp.fetch_add(1, Ordering::Relaxed);
                return;
            }

            match client
                .get(&url)
                .header("Host", &warm_url.host)
                .header("User-Agent", "Nozdormu-Warm/1.0")
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let headers: Vec<(String, String)> = resp
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect();
                    match resp.bytes().await {
                        Ok(body) => {
                            let cc = headers
                                .iter()
                                .find(|(k, _)| k == "cache-control")
                                .map(|(_, v)| v.as_str());
                            let expires_h = headers
                                .iter()
                                .find(|(k, _)| k == "expires")
                                .map(|(_, v)| v.as_str());
                            let ttl = cdn_cache::strategy::adjust_ttl(
                                site.cache.default_ttl,
                                cc,
                                expires_h,
                            );
                            let swr = cdn_cache::strategy::parse_stale_while_revalidate(cc);
                            let tags = cdn_cache::storage::parse_cache_tags(&headers);
                            let meta = cdn_cache::storage::build_cache_meta(
                                status,
                                &headers,
                                ttl,
                                body.len() as u64,
                                swr,
                                tags,
                            );
                            if let Err(e) =
                                storage.put(&sid, &cache_key, &meta, body.to_vec()).await
                            {
                                log::warn!("[Warm] cache write failed: {}", e);
                                fail.fetch_add(1, Ordering::Relaxed);
                            } else {
                                comp.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            log::warn!("[Warm] body read failed: {}", e);
                            fail.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("[Warm] fetch failed for {}: {}", url, e);
                    fail.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    // Wait for all tasks
    for h in handles {
        let _ = h.await;
    }

    let final_comp = completed.load(Ordering::Relaxed);
    let final_fail = failed.load(Ordering::Relaxed);
    let elapsed = start.elapsed().as_secs_f64();

    task_tracker.update_completed(&task_id, final_comp, final_fail);

    CACHE_WARM_TOTAL
        .with_label_values(&[site_id.as_str(), "ok"])
        .inc();
    CACHE_WARM_DURATION
        .with_label_values(&[site_id.as_str()])
        .observe(elapsed);

    log::info!(
        "[Warm] completed: site={} completed={} failed={} duration={:.2}s",
        site_id,
        final_comp,
        final_fail,
        elapsed
    );
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warm_request_deserialization() {
        let json = r#"{
            "site_id": "my-site",
            "urls": [
                {"host": "example.com", "path": "/page1"},
                {"host": "example.com", "path": "/page2", "query_string": "v=1"}
            ]
        }"#;
        let req: WarmRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.site_id, "my-site");
        assert_eq!(req.urls.len(), 2);
        assert_eq!(req.urls[0].path, "/page1");
        assert_eq!(req.urls[1].query_string, Some("v=1".to_string()));
    }

    #[test]
    fn test_warm_task_tracker_lifecycle() {
        let tracker = WarmTaskTracker::new();

        let status = WarmTaskStatus {
            task_id: "task-1".to_string(),
            site_id: "site-1".to_string(),
            status: WarmTaskState::Running,
            urls_total: 10,
            urls_completed: 0,
            urls_failed: 0,
            started_at: Utc::now().timestamp(),
            completed_at: None,
            error: None,
        };
        tracker.insert(status);

        let got = tracker.get("task-1").unwrap();
        assert!(matches!(got.status, WarmTaskState::Running));

        tracker.update_completed("task-1", 8, 2);
        let got = tracker.get("task-1").unwrap();
        assert!(matches!(got.status, WarmTaskState::Completed));
        assert_eq!(got.urls_completed, 8);
        assert_eq!(got.urls_failed, 2);
    }

    #[test]
    fn test_warm_task_tracker_failed() {
        let tracker = WarmTaskTracker::new();

        let status = WarmTaskStatus {
            task_id: "task-2".to_string(),
            site_id: "site-1".to_string(),
            status: WarmTaskState::Running,
            urls_total: 5,
            urls_completed: 0,
            urls_failed: 0,
            started_at: Utc::now().timestamp(),
            completed_at: None,
            error: None,
        };
        tracker.insert(status);

        tracker.update_failed("task-2", "no origin".to_string());
        let got = tracker.get("task-2").unwrap();
        assert!(matches!(got.status, WarmTaskState::Failed));
        assert_eq!(got.error, Some("no origin".to_string()));
    }
}
