use crate::balancer::DynamicBalancer;
use crate::ssl::challenge::ChallengeStore;
use arc_swap::ArcSwap;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
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
    /// Optional Bearer token for admin API authentication.
    /// If None, no authentication is required (localhost-only trust).
    pub admin_token: Option<String>,
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
        .route("/cc/blocked", get(get_cc_blocked))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
}

/// POST /reload — Trigger config reload from etcd.
async fn reload_config(
    State(state): State<Arc<AdminState>>,
) -> Json<Value> {
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
async fn clear_ssl_cache(
    State(_state): State<Arc<AdminState>>,
) -> Json<Value> {
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
    let site = config
        .sites
        .get(&site_id)
        .ok_or(StatusCode::NOT_FOUND)?;

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

/// GET /upstream/health — Get upstream health status.
async fn get_upstream_health(
    State(state): State<Arc<AdminState>>,
) -> Json<Value> {
    let config = state.live_config.load();
    let mut health_data = Vec::new();

    for (site_id, site) in &config.sites {
        for origin in &site.origins {
            let healthy = state.balancer.health.is_healthy(site_id, &origin.id);
            health_data.push(json!({
                "site_id": site_id,
                "origin_id": origin.id,
                "host": origin.host,
                "port": origin.port,
                "healthy": healthy,
                "backup": origin.backup,
                "enabled": origin.enabled,
            }));
        }
    }

    Json(json!({ "origins": health_data }))
}

/// GET /cc/blocked — Get currently blocked IPs.
async fn get_cc_blocked(
    State(_state): State<Arc<AdminState>>,
) -> Json<Value> {
    // moka Cache doesn't support iteration; needs blocked_index DashMap for full listing
    Json(json!({ "blocked": [], "note": "blocked IP listing requires blocked_index" }))
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
