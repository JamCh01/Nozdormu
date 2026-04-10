use crate::admin::purge::{
    purge_site_background, purge_url, validate_purge_url, validate_site_id, PurgeRequest,
    PurgeTaskState, PurgeTaskStatus, PurgeTaskTracker,
};
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
}

/// Constant-time byte comparison using ring.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    ring::constant_time::verify_slices_are_equal(a, b).is_ok()
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
