use crate::admin::purge::{
    purge_site_background, purge_url, validate_purge_url, validate_site_id, PurgeRequest,
    PurgeTaskState, PurgeTaskStatus, PurgeTaskTracker,
};
use crate::balancer::DynamicBalancer;
use crate::ssl::challenge::ChallengeStore;
use crate::utils::redis_pool::RedisPool;
use arc_swap::ArcSwap;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post, put};
use axum::Router;
use cdn_cache::storage::CacheStorage;
use cdn_config::{EtcdConfigManager, LiveConfig};
use cdn_middleware::cc::CcEngine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Shared state for admin API handlers.
pub struct AdminState {
    pub live_config: Arc<ArcSwap<LiveConfig>>,
    pub balancer: Arc<DynamicBalancer>,
    pub cc_engine: Arc<CcEngine>,
    pub challenge_store: Arc<ChallengeStore>,
    pub etcd_manager: Arc<EtcdConfigManager>,
    /// Optional Bearer token for admin API authentication.
    /// If None, no authentication is required (localhost-only trust).
    pub admin_token: Option<String>,
    pub cache_storage: Arc<CacheStorage>,
    pub redis_pool: Arc<RedisPool>,
    pub purge_tracker: Arc<PurgeTaskTracker>,
}

/// Bearer token authentication middleware.
/// Skips auth if no token is configured (backward compatible).
/// Uses constant-time comparison to prevent timing attacks.
async fn auth_middleware(
    State(state): State<Arc<AdminState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    if let Some(ref expected_token) = state.admin_token {
        let auth_header = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok());
        let provided = auth_header.and_then(|h| h.strip_prefix("Bearer "));
        let matches = match provided {
            Some(token) => constant_time_eq(token.as_bytes(), expected_token.as_bytes()),
            None => false,
        };
        if !matches {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"status": "error", "message": "unauthorized"})),
            )
                .into_response();
        }
    }
    next.run(req).await.into_response()
}

/// Constant-time byte comparison using ring (cryptographically audited).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    ring::constant_time::verify_slices_are_equal(a, b).is_ok()
}

/// Build the Axum router for the admin API.
pub fn admin_router(state: Arc<AdminState>) -> Router {
    Router::new()
        .route("/reload", post(reload_config))
        .route("/ssl/clear-cache", post(clear_ssl_cache))
        .route("/site/:id", get(get_site_config))
        .route("/upstream/health", get(get_upstream_health))
        .route(
            "/upstream/health/:site_id/:origin_id",
            put(set_upstream_health),
        )
        .route("/cc/blocked", get(get_cc_blocked))
        .route("/cache/purge", post(purge_cache))
        .route("/cache/purge/status/:task_id", get(purge_status))
        .route("/cache/purge/status", get(purge_list_tasks))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

/// POST /reload — Trigger config reload from etcd.
async fn reload_config(State(state): State<Arc<AdminState>>) -> Json<Value> {
    log::info!("[Admin] config reload requested");
    match state.etcd_manager.load_all().await {
        Ok(rev) => {
            log::info!("[Admin] config reloaded at revision {}", rev);
            Json(json!({
                "status": "ok",
                "message": "config reloaded",
                "revision": rev,
                "sites_loaded": state.live_config.load().sites.len(),
            }))
        }
        Err(e) => {
            log::error!("[Admin] config reload failed: {}", e);
            Json(json!({
                "status": "error",
                "message": format!("reload failed: {}", e),
            }))
        }
    }
}

/// POST /ssl/clear-cache — Clear SSL certificate cache.
async fn clear_ssl_cache(State(_state): State<Arc<AdminState>>) -> Json<Value> {
    // TODO: cert_manager.clear_cache() when CertManager is wired in
    log::info!("[Admin] SSL cache clear requested");
    Json(json!({ "status": "ok", "message": "ssl cache cleared" }))
}

/// GET /site/{id} — Get site configuration.
async fn get_site_config(
    State(state): State<Arc<AdminState>>,
    Path(site_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let config = state.live_config.load();
    let site = config.sites.get(&site_id).ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(json!({
        "site_id": site.site_id,
        "enabled": site.enabled,
        "domains": site.domains,
        "origins_count": site.origins.len(),
        "waf_enabled": site.waf.enabled,
        "cc_enabled": site.cc.enabled,
        "cache_enabled": site.cache.enabled,
    })))
}

/// GET /upstream/health — Get upstream health status with details.
async fn get_upstream_health(State(state): State<Arc<AdminState>>) -> Json<Value> {
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

    Json(json!({ "origins": health_data }))
}

#[derive(Deserialize)]
struct SetHealthBody {
    healthy: bool,
}

/// PUT /upstream/health/:site_id/:origin_id — Manually set origin health status.
async fn set_upstream_health(
    State(state): State<Arc<AdminState>>,
    Path((site_id, origin_id)): Path<(String, String)>,
    Json(body): Json<SetHealthBody>,
) -> Json<Value> {
    state
        .balancer
        .health
        .set_status(&site_id, &origin_id, body.healthy);
    log::info!(
        "[Admin] health override: site={} origin={} healthy={}",
        site_id,
        origin_id,
        body.healthy
    );
    Json(json!({
        "status": "ok",
        "site_id": site_id,
        "origin_id": origin_id,
        "healthy": body.healthy,
    }))
}

/// GET /cc/blocked — Get currently blocked IPs.
async fn get_cc_blocked(State(_state): State<Arc<AdminState>>) -> Json<Value> {
    // moka Cache doesn't support iteration; needs blocked_index DashMap for full listing
    Json(json!({ "blocked": [], "note": "blocked IP listing requires blocked_index" }))
}

/// POST /cache/purge — Purge cache entries.
async fn purge_cache(
    State(state): State<Arc<AdminState>>,
    Json(req): Json<PurgeRequest>,
) -> (StatusCode, Json<Value>) {
    match req {
        PurgeRequest::Url {
            site_id,
            host,
            path,
            query_string,
            sort_query_string,
            vary_headers,
        } => {
            // Validate inputs
            if let Err(e) = validate_site_id(&site_id) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "message": e})),
                );
            }
            if let Err(e) = validate_purge_url(&host, &path) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "message": e})),
                );
            }

            // Validate site exists
            if !state.live_config.load().sites.contains_key(&site_id) {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "status": "error", "message": format!("site '{}' not found", site_id)
                    })),
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
                    StatusCode::OK,
                    Json(json!({
                        "status": "ok",
                        "purge_type": "url",
                        "site_id": site_id,
                        "cache_key": cache_key,
                        "keys_deleted": 1,
                    })),
                ),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "status": "error",
                        "message": format!("purge failed: {}", e),
                    })),
                ),
            }
        }
        PurgeRequest::Site { site_id } => {
            // Validate inputs
            if let Err(e) = validate_site_id(&site_id) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "message": e})),
                );
            }

            // Validate site exists
            if !state.live_config.load().sites.contains_key(&site_id) {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "status": "error", "message": format!("site '{}' not found", site_id)
                    })),
                );
            }

            // Check for active purge on same site
            if state.purge_tracker.has_active_for_site(&site_id) {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "status": "error",
                        "message": format!("a purge is already running for site '{}'", site_id),
                    })),
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

            // Spawn background task
            let redis_pool = Arc::clone(&state.redis_pool);
            let cache_storage = Arc::clone(&state.cache_storage);
            let tracker = Arc::clone(&state.purge_tracker);
            let tid = task_id.clone();
            let sid = site_id.clone();
            tokio::spawn(async move {
                purge_site_background(redis_pool, cache_storage, sid, tracker, tid).await;
            });

            (
                StatusCode::ACCEPTED,
                Json(json!({
                    "status": "accepted",
                    "purge_type": "site",
                    "site_id": site_id,
                    "task_id": task_id,
                })),
            )
        }
    }
}

/// GET /cache/purge/status/:task_id — Get purge task status.
async fn purge_status(
    State(state): State<Arc<AdminState>>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let task = state
        .purge_tracker
        .get(&task_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(
        serde_json::to_value(task).unwrap_or(json!({"status": "error"})),
    ))
}

/// GET /cache/purge/status — List all purge tasks.
async fn purge_list_tasks(State(state): State<Arc<AdminState>>) -> Json<Value> {
    let tasks = state.purge_tracker.list();
    Json(json!({ "tasks": tasks }))
}

/// Start the admin API server on 127.0.0.1:8080.
/// This should be spawned as a tokio task.
pub async fn start_admin_server(state: Arc<AdminState>) {
    let app = admin_router(state);
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:8080").await {
        Ok(l) => l,
        Err(e) => {
            log::error!("[Admin] failed to bind 127.0.0.1:8080: {}", e);
            return;
        }
    };
    log::info!("[Admin] API listening on 127.0.0.1:8080");
    if let Err(e) = axum::serve(listener, app).await {
        log::error!("[Admin] server error: {}", e);
    }
}
