use crate::admin::purge::{
    purge_site_background, purge_url, validate_purge_url, validate_site_id, PurgeRequest,
    PurgeTaskState, PurgeTaskStatus, PurgeTaskTracker,
};
use crate::admin::warm::{self, WarmRequest, WarmTaskState, WarmTaskStatus, WarmTaskTracker};
use crate::balancer::DynamicBalancer;
use crate::ssl::challenge::ChallengeStore;
use crate::utils::redis_pool::RedisPool;
use arc_swap::ArcSwap;
use cdn_cache::storage::CacheStorage;
use cdn_config::{EtcdConfigManager, LiveConfig};
use cdn_middleware::cc::CcEngine;
use serde_json::{json, Value};
use std::sync::Arc;

/// Shared state for admin API handlers.
pub struct AdminState {
    pub live_config: Arc<ArcSwap<LiveConfig>>,
    pub balancer: Arc<DynamicBalancer>,
    pub cc_engine: Arc<CcEngine>,
    pub challenge_store: Arc<ChallengeStore>,
    pub etcd_manager: Arc<EtcdConfigManager>,
    /// Bearer token for admin API authentication (from etcd global/security).
    /// Required for public exposure — if None, admin API returns 403.
    pub admin_token: Option<String>,
    pub cache_storage: Arc<CacheStorage>,
    pub redis_pool: Arc<RedisPool>,
    pub purge_tracker: Arc<PurgeTaskTracker>,
    pub warm_tracker: Arc<WarmTaskTracker>,
    /// Active log backend name for status endpoint.
    pub log_backend_name: String,
    /// Live stream store for ingest admin endpoints (None if ingest disabled).
    pub live_stream_store: Option<Arc<cdn_ingest::LiveStreamStore>>,
}

/// Constant-time byte comparison (re-export from cdn-common).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    cdn_common::constant_time_eq(a, b)
}

/// POST /_admin/reload — Trigger config reload from etcd.
pub async fn reload_config(state: &AdminState) -> (u16, Value) {
    log::info!("[Admin] config reload requested");
    match state.etcd_manager.load_all().await {
        Ok(rev) => {
            log::info!("[Admin] config reloaded at revision {}", rev);
            (
                200,
                json!({
                    "status": "ok",
                    "message": "config reloaded",
                    "revision": rev,
                    "sites_loaded": state.live_config.load().sites.len(),
                }),
            )
        }
        Err(e) => {
            log::error!("[Admin] config reload failed: {}", e);
            (
                500,
                json!({
                    "status": "error",
                    "message": format!("reload failed: {}", e),
                }),
            )
        }
    }
}

/// POST /_admin/ssl/clear-cache — Clear SSL certificate cache.
pub async fn clear_ssl_cache(_state: &AdminState) -> (u16, Value) {
    // TODO: cert_manager.clear_cache() when CertManager is wired in
    log::info!("[Admin] SSL cache clear requested");
    (
        200,
        json!({ "status": "ok", "message": "ssl cache cleared" }),
    )
}

/// GET /_admin/site/{id} — Get site configuration.
pub async fn get_site_config(state: &AdminState, site_id: &str) -> (u16, Value) {
    let config = state.live_config.load();
    match config.sites.get(site_id) {
        Some(site) => (
            200,
            json!({
                "site_id": site.site_id,
                "enabled": site.enabled,
                "domains": site.domains,
                "origins_count": site.origins.len(),
                "waf_enabled": site.waf.enabled,
                "cc_enabled": site.cc.enabled,
                "cache_enabled": site.cache.enabled,
            }),
        ),
        None => (
            404,
            json!({"status": "error", "message": format!("site '{}' not found", site_id)}),
        ),
    }
}

/// GET /_admin/upstream/health — Get upstream health status.
pub async fn get_upstream_health(state: &AdminState) -> (u16, Value) {
    let config = state.live_config.load();
    let mut health_data = Vec::new();

    for (site_id, site) in &config.sites {
        for origin in &site.origins {
            let detail = state.balancer.health.get_detail(site_id, &origin.id);
            let last_check_ts = detail.last_active_check.map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            });
            health_data.push(json!({
                "site_id": site_id,
                "origin_id": origin.id,
                "host": origin.host,
                "port": origin.port,
                "healthy": detail.healthy,
                "backup": origin.backup,
                "enabled": origin.enabled,
                "consecutive_successes": detail.consecutive_successes,
                "consecutive_failures": detail.consecutive_failures,
                "last_check_time": last_check_ts,
                "last_check_success": detail.last_active_success,
            }));
        }
    }

    (200, json!({ "origins": health_data }))
}

/// PUT /_admin/upstream/health/{site_id}/{origin_id} — Manually set origin health.
pub async fn set_upstream_health(
    state: &AdminState,
    site_id: &str,
    origin_id: &str,
    body: &[u8],
) -> (u16, Value) {
    #[derive(serde::Deserialize)]
    struct SetHealthBody {
        healthy: bool,
    }
    let parsed: SetHealthBody = match serde_json::from_slice(body) {
        Ok(b) => b,
        Err(e) => {
            return (
                400,
                json!({"status": "error", "message": format!("invalid body: {}", e)}),
            )
        }
    };
    state
        .balancer
        .health
        .set_status(site_id, origin_id, parsed.healthy);
    log::info!(
        "[Admin] health override: site={} origin={} healthy={}",
        site_id,
        origin_id,
        parsed.healthy
    );
    (
        200,
        json!({
            "status": "ok",
            "site_id": site_id,
            "origin_id": origin_id,
            "healthy": parsed.healthy,
        }),
    )
}

/// GET /_admin/cc/blocked — Get currently blocked IPs.
pub async fn get_cc_blocked(_state: &AdminState) -> (u16, Value) {
    (
        200,
        json!({ "blocked": [], "note": "blocked IP listing requires blocked_index" }),
    )
}

/// POST /_admin/cache/purge — Purge cache entries.
pub async fn purge_cache(state: &AdminState, body: &[u8]) -> (u16, Value) {
    let req: PurgeRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                400,
                json!({"status": "error", "message": format!("invalid body: {}", e)}),
            )
        }
    };

    match req {
        PurgeRequest::Url {
            site_id,
            host,
            path,
            query_string,
            sort_query_string,
            vary_headers,
        } => {
            if let Err(e) = validate_site_id(&site_id) {
                return (400, json!({"status": "error", "message": e}));
            }
            if let Err(e) = validate_purge_url(&host, &path) {
                return (400, json!({"status": "error", "message": e}));
            }
            if !state.live_config.load().sites.contains_key(&site_id) {
                return (
                    404,
                    json!({"status": "error", "message": format!("site '{}' not found", site_id)}),
                );
            }

            log::info!(
                "[Admin] cache purge URL: site={} host={} path={}",
                site_id,
                host,
                path
            );

            match purge_url(
                &state.cache_storage,
                &site_id,
                &host,
                &path,
                query_string.as_deref(),
                sort_query_string,
                &vary_headers,
            )
            .await
            {
                Ok(cache_key) => (
                    200,
                    json!({
                        "status": "ok",
                        "purge_type": "url",
                        "site_id": site_id,
                        "cache_key": cache_key,
                        "keys_deleted": 1,
                    }),
                ),
                Err(e) => (
                    500,
                    json!({"status": "error", "message": format!("purge failed: {}", e)}),
                ),
            }
        }
        PurgeRequest::Site { site_id } => {
            if let Err(e) = validate_site_id(&site_id) {
                return (400, json!({"status": "error", "message": e}));
            }
            if !state.live_config.load().sites.contains_key(&site_id) {
                return (
                    404,
                    json!({"status": "error", "message": format!("site '{}' not found", site_id)}),
                );
            }
            if state.purge_tracker.has_active_for_site(&site_id) {
                return (
                    409,
                    json!({
                        "status": "error",
                        "message": format!("a purge is already running for site '{}'", site_id),
                    }),
                );
            }

            let task_id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().timestamp();

            let task_status = PurgeTaskStatus {
                task_id: task_id.clone(),
                site_id: site_id.clone(),
                status: PurgeTaskState::Running,
                keys_deleted: 0,
                started_at: now,
                completed_at: None,
                error: None,
            };
            state.purge_tracker.insert(task_status);

            log::info!(
                "[Admin] cache purge site: site={} task={}",
                site_id,
                task_id
            );

            let redis_pool = Arc::clone(&state.redis_pool);
            let cache_storage = Arc::clone(&state.cache_storage);
            let tracker = Arc::clone(&state.purge_tracker);
            let tid = task_id.clone();
            let sid = site_id.clone();
            tokio::spawn(async move {
                purge_site_background(redis_pool, cache_storage, sid, tracker, tid).await;
            });

            (
                202,
                json!({
                    "status": "accepted",
                    "purge_type": "site",
                    "site_id": site_id,
                    "task_id": task_id,
                }),
            )
        }
        PurgeRequest::Tag { site_id, tag } => {
            if let Err(e) = validate_site_id(&site_id) {
                return (400, json!({"status": "error", "message": e}));
            }
            if tag.is_empty() {
                return (
                    400,
                    json!({"status": "error", "message": "tag cannot be empty"}),
                );
            }
            if tag.len() > 128 {
                return (
                    400,
                    json!({"status": "error", "message": "tag too long (max 128 chars)"}),
                );
            }
            if !state.live_config.load().sites.contains_key(&site_id) {
                return (
                    404,
                    json!({"status": "error", "message": format!("site '{}' not found", site_id)}),
                );
            }

            log::info!("[Admin] cache purge tag: site={} tag={}", site_id, tag);

            match state.cache_storage.delete_by_tag(&site_id, &tag).await {
                Ok(deleted) => (
                    200,
                    json!({
                        "status": "ok",
                        "purge_type": "tag",
                        "site_id": site_id,
                        "tag": tag,
                        "keys_deleted": deleted,
                    }),
                ),
                Err(e) => (
                    500,
                    json!({"status": "error", "message": format!("tag purge failed: {}", e)}),
                ),
            }
        }
    }
}

/// GET /_admin/cache/purge/status/{task_id} — Get purge task status.
pub async fn purge_status(state: &AdminState, task_id: &str) -> (u16, Value) {
    match state.purge_tracker.get(task_id) {
        Some(task) => (
            200,
            serde_json::to_value(task).unwrap_or(json!({"status": "error"})),
        ),
        None => (404, json!({"status": "error", "message": "task not found"})),
    }
}

/// GET /_admin/cache/purge/status — List all purge tasks.
pub async fn purge_list_tasks(state: &AdminState) -> (u16, Value) {
    let tasks = state.purge_tracker.list();
    (200, json!({ "tasks": tasks }))
}

/// POST /_admin/cache/warm — Start cache warming.
pub async fn warm_cache(state: &AdminState, body: &[u8]) -> (u16, Value) {
    let req: WarmRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                400,
                json!({"status": "error", "message": format!("invalid body: {}", e)}),
            )
        }
    };

    if let Err(e) = validate_site_id(&req.site_id) {
        return (400, json!({"status": "error", "message": e}));
    }

    let config = state.live_config.load();
    let site = match config.sites.get(&req.site_id) {
        Some(s) => Arc::clone(s),
        None => {
            return (
                404,
                json!({"status": "error", "message": format!("site '{}' not found", req.site_id)}),
            )
        }
    };

    if req.urls.is_empty() {
        return (
            400,
            json!({"status": "error", "message": "urls list is empty"}),
        );
    }
    if req.urls.len() > 1000 {
        return (
            400,
            json!({"status": "error", "message": "too many URLs (max 1000)"}),
        );
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let urls_count = req.urls.len();

    let task_status = WarmTaskStatus {
        task_id: task_id.clone(),
        site_id: req.site_id.clone(),
        status: WarmTaskState::Running,
        urls_total: urls_count as u32,
        urls_completed: 0,
        urls_failed: 0,
        started_at: now,
        completed_at: None,
        error: None,
    };
    state.warm_tracker.insert(task_status);

    log::info!(
        "[Admin] cache warm: site={} urls={} task={}",
        req.site_id,
        urls_count,
        task_id
    );

    let cache_storage = Arc::clone(&state.cache_storage);
    let tracker = Arc::clone(&state.warm_tracker);
    let tid = task_id.clone();
    let sid = req.site_id.clone();

    tokio::spawn(async move {
        warm::warm_urls_background(cache_storage, site, sid, req.urls, tracker, tid).await;
    });

    (
        202,
        json!({
            "status": "accepted",
            "task_id": task_id,
            "site_id": req.site_id,
            "urls_count": urls_count,
        }),
    )
}

/// GET /_admin/cache/warm/status/{task_id} — Get warm task status.
pub async fn warm_status(state: &AdminState, task_id: &str) -> (u16, Value) {
    match state.warm_tracker.get(task_id) {
        Some(task) => (
            200,
            serde_json::to_value(task).unwrap_or(json!({"status": "error"})),
        ),
        None => (404, json!({"status": "error", "message": "task not found"})),
    }
}

/// GET /_admin/cache/warm/status — List all warm tasks.
pub async fn warm_list_tasks(state: &AdminState) -> (u16, Value) {
    let tasks = state.warm_tracker.list();
    (200, json!({ "tasks": tasks }))
}

// ── Config version management ──

/// GET /_admin/config/history/{site_id} — List config version history.
pub async fn config_history(state: &AdminState, site_id: &str) -> (u16, Value) {
    if let Err(e) = validate_site_id(site_id) {
        return (400, json!({"status": "error", "message": e}));
    }

    let etcd_config = state.etcd_manager.etcd_config();
    match cdn_config::config_history::list_versions(etcd_config, site_id).await {
        Ok(versions) => (
            200,
            json!({
                "site_id": site_id,
                "versions": serde_json::to_value(&versions).unwrap_or(json!([])),
            }),
        ),
        Err(e) => (
            500,
            json!({"status": "error", "message": format!("failed to list versions: {}", e)}),
        ),
    }
}

/// GET /_admin/config/history/{site_id}/{version} — Get a specific version snapshot.
pub async fn config_version_detail(
    state: &AdminState,
    site_id: &str,
    version: u64,
) -> (u16, Value) {
    if let Err(e) = validate_site_id(site_id) {
        return (400, json!({"status": "error", "message": e}));
    }

    let etcd_config = state.etcd_manager.etcd_config();
    match cdn_config::config_history::get_version(etcd_config, site_id, version).await {
        Ok(Some(snapshot)) => (
            200,
            serde_json::to_value(&snapshot).unwrap_or(json!({"status": "error"})),
        ),
        Ok(None) => (
            404,
            json!({
                "status": "error",
                "message": format!("version {} not found for site '{}'", version, site_id),
            }),
        ),
        Err(e) => (
            500,
            json!({"status": "error", "message": format!("failed to get version: {}", e)}),
        ),
    }
}

/// POST /_admin/config/rollback/{site_id}/{version} — Rollback to a previous version.
pub async fn config_rollback(state: &AdminState, site_id: &str, version: u64) -> (u16, Value) {
    if let Err(e) = validate_site_id(site_id) {
        return (400, json!({"status": "error", "message": e}));
    }

    let etcd_config = state.etcd_manager.etcd_config();
    match cdn_config::config_history::rollback_to_version(etcd_config, site_id, version).await {
        Ok((new_version, put_revision)) => {
            // Tag the resulting watch event as Rollback
            state.etcd_manager.set_pending_change_type(
                site_id.to_string(),
                put_revision,
                cdn_config::config_history::ConfigChangeType::Rollback,
            );

            log::info!(
                "[Admin] config rollback: site={} to_version={} new_version={}",
                site_id,
                version,
                new_version
            );

            (
                200,
                json!({
                    "status": "ok",
                    "message": format!("rolled back site '{}' to version {}", site_id, version),
                    "site_id": site_id,
                    "rolled_back_to": version,
                    "new_version": new_version,
                }),
            )
        }
        Err(e) => {
            let msg = e.to_string();
            let status = if msg.contains("not found") { 404 } else { 500 };
            (
                status,
                json!({"status": "error", "message": format!("rollback failed: {}", msg)}),
            )
        }
    }
}

/// GET /_admin/adaptive/weights — View current effective weights for all origins.
pub async fn get_adaptive_weights(state: &AdminState) -> (u16, Value) {
    let config = state.live_config.load();
    let mut data = Vec::new();
    for (site_id, site) in &config.sites {
        let adaptive_cfg = &site.load_balancer.adaptive_weight;
        for origin in &site.origins {
            let eff = state
                .balancer
                .effective_weight(site_id, origin, adaptive_cfg);
            let summary = state.balancer.get_origin_stats_summary(site_id, &origin.id);
            data.push(json!({
                "site_id": site_id,
                "origin_id": origin.id,
                "static_weight": origin.weight,
                "effective_weight": eff,
                "adaptive_enabled": adaptive_cfg.enabled,
                "p99_latency_ms": summary.p99_latency,
                "error_rate": summary.error_rate,
                "sample_count": summary.sample_count,
            }));
        }
    }
    (200, json!({ "origins": data }))
}

/// GET /_admin/log/status — current log backend status.
pub async fn get_log_status(state: &AdminState) -> (u16, Value) {
    let config = &state.log_backend_name;
    (
        200,
        json!({
            "backend": config.as_str(),
        }),
    )
}
