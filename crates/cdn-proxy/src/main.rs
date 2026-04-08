#[cfg(all(not(windows), not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

use arc_swap::ArcSwap;
use cdn_config::{load_cdn_config, BootstrapConfig, LiveConfig, NodeConfig};
use cdn_middleware::cc::CcEngine;
use cdn_middleware::waf::WafEngine;
use cdn_proxy::admin::{admin_router, AdminState};
use cdn_proxy::balancer::DynamicBalancer;
use cdn_proxy::dns::DnsResolver;
use cdn_proxy::health::HealthChecker;
use cdn_proxy::logging::queue::init_log_queue;
use cdn_proxy::proxy::CdnProxy;
use cdn_proxy::ssl::challenge::ChallengeStore;
use cdn_proxy::utils::redis_pool::RedisPool;
use pingora::prelude::*;
use pingora::services::listening::Service as ListeningService;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    env_logger::init();

    // ── 1. Bootstrap: read env-only config (node identity + etcd address + paths) ──
    let bootstrap = BootstrapConfig::from_env();
    log::info!(
        "[Bootstrap] Node ID: {}, Labels: {:?}, Env: {}",
        bootstrap.node.id,
        bootstrap.node.labels,
        bootstrap.node.env
    );

    // ── 2. Load global config from etcd, then build full NodeConfig ──
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to create init runtime");

    let global_config = rt.block_on(cdn_config::load_global_config(&bootstrap.etcd));

    let node_config = NodeConfig::from_etcd_and_env(&global_config);
    if let Err(errors) = node_config.validate() {
        for e in &errors {
            log::error!("[Config] {}", e);
        }
        log::warn!("[Config] validation failed, continuing with defaults");
    }
    node_config.print_summary();

    // ── 3. Pingora server bootstrap ──
    let opt = Opt::parse_args();
    let config_path = opt
        .conf
        .clone()
        .unwrap_or_else(|| "config/default.yaml".to_string());
    let cdn_config =
        load_cdn_config(Path::new(&config_path)).expect("failed to load CDN config");

    let mut server = Server::new(Some(opt)).expect("failed to create server");
    server.bootstrap();

    // ── 4. Async initialization (Redis + etcd sites) ──
    let live_config = Arc::new(ArcSwap::from_pointee(LiveConfig::default()));

    // Connect to Redis
    let redis_pool = Arc::new(rt.block_on(RedisPool::connect(&node_config)));
    log::info!("[Init] Redis: {}", redis_pool.describe());

    // Initialize log queue with Redis pool
    init_log_queue(Arc::clone(&redis_pool));

    // Load initial config from etcd
    let etcd_manager = Arc::new(cdn_config::EtcdConfigManager::new(
        node_config.etcd.clone(),
        node_config.node.labels.clone(),
        Arc::clone(&live_config),
    ));
    match rt.block_on(etcd_manager.load_all()) {
        Ok(rev) => log::info!("[Init] etcd initial load complete at revision {}", rev),
        Err(e) => log::warn!("[Init] etcd initial load failed: {}, starting with empty config", e),
    }

    drop(rt);

    // ── 5. Static upstream (temporary fallback) ──
    let upstream_addrs: Vec<String> = cdn_config
        .upstreams
        .iter()
        .map(|u| u.address.clone())
        .collect();
    let addr_refs: Vec<&str> = upstream_addrs.iter().map(|s| s.as_str()).collect();

    let mut upstreams =
        LoadBalancer::try_from_iter(addr_refs).expect("failed to create load balancer");

    let hc = TcpHealthCheck::new();
    upstreams.set_health_check(hc);
    upstreams.health_check_frequency =
        Some(Duration::from_secs(cdn_config.health_check.interval_secs));

    let background = background_service("health check", upstreams);
    let lb = background.task();

    let sni = cdn_config
        .upstreams
        .iter()
        .find(|u| u.tls)
        .and_then(|u| u.sni.clone())
        .unwrap_or_default();

    let has_tls = cdn_config.upstreams.iter().any(|u| u.tls);

    // ── 6. Create proxy service ──
    let waf_engine = Arc::new(WafEngine::new(Path::new(&node_config.paths.geoip)));

    // CC engine with optional Redis for distributed counter sync
    let redis_ops: Option<Arc<dyn cdn_common::RedisOps>> = if redis_pool.is_available() {
        Some(Arc::clone(&redis_pool) as Arc<dyn cdn_common::RedisOps>)
    } else {
        None
    };
    let cc_engine = Arc::new(CcEngine::new(
        &node_config.security.cc_challenge_secret,
        node_config.security.cc_default_rate,
        node_config.security.cc_default_window,
        node_config.security.cc_default_block_duration,
        redis_ops,
    ));

    let health_checker = Arc::new(HealthChecker::new(
        node_config.balancer.unhealthy_threshold,
        node_config.balancer.healthy_threshold,
    ));
    let dns_resolver = Arc::new(DnsResolver::new());
    let dynamic_balancer = Arc::new(DynamicBalancer::new(
        health_checker,
        dns_resolver,
    ));

    let challenge_store = Arc::new(ChallengeStore::new());

    // ── Admin API state ──
    let admin_token = std::env::var("CDN_ADMIN_TOKEN").ok().filter(|t| !t.is_empty());
    let admin_state = Arc::new(AdminState {
        live_config: Arc::clone(&live_config),
        balancer: Arc::clone(&dynamic_balancer),
        cc_engine: Arc::clone(&cc_engine),
        challenge_store: Arc::clone(&challenge_store),
        etcd_manager: Arc::clone(&etcd_manager),
        admin_token,
    });

    let cdn_proxy = CdnProxy {
        lb,
        sni,
        tls: has_tls,
        live_config,
        waf_engine,
        cc_engine,
        balancer: dynamic_balancer,
        challenge_store,
        redis_pool,
        trusted_proxies: node_config.security.trusted_proxies.clone(),
        default_compression: node_config.compression.clone(),
    };

    let mut proxy_service = http_proxy_service(&server.configuration, cdn_proxy);
    proxy_service.add_tcp(&cdn_config.listen);
    log::info!("proxy listening on {}", cdn_config.listen);

    // ── 7. Prometheus metrics ──
    let mut prometheus_service = ListeningService::prometheus_http_service();
    prometheus_service.add_tcp(&cdn_config.metrics_listen);
    log::info!("metrics listening on {}", cdn_config.metrics_listen);

    // ── 8. Background services ──

    // Admin API on 127.0.0.1:8080
    let admin_bg = {
        let admin_state = Arc::clone(&admin_state);
        background_service("admin api", AdminBgService { state: admin_state })
    };

    // etcd watch loop for live config updates
    let etcd_bg = {
        let mgr = Arc::clone(&etcd_manager);
        background_service("etcd watch", EtcdWatchBgService { manager: mgr })
    };

    // ── 9. Register and run ──
    server.add_service(background);
    server.add_service(proxy_service);
    server.add_service(prometheus_service);
    server.add_service(admin_bg);
    server.add_service(etcd_bg);

    log::info!("Nozdormu CDN starting...");
    server.run_forever();
}

// ── Background service wrappers ──

use async_trait::async_trait;
use pingora::services::background::BackgroundService;
use pingora::server::ShutdownWatch;

struct AdminBgService {
    state: Arc<AdminState>,
}

#[async_trait]
impl BackgroundService for AdminBgService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let state = Arc::clone(&self.state);
        let app = admin_router(state);
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:8080").await {
            Ok(l) => l,
            Err(e) => {
                log::error!("[Admin] failed to bind 127.0.0.1:8080: {}", e);
                return;
            }
        };
        log::info!("[Admin] API listening on 127.0.0.1:8080");
        let graceful = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.changed().await;
                log::info!("[Admin] shutting down");
            });
        if let Err(e) = graceful.await {
            log::error!("[Admin] server error: {}", e);
        }
    }
}

struct EtcdWatchBgService {
    manager: Arc<cdn_config::EtcdConfigManager>,
}

#[async_trait]
impl BackgroundService for EtcdWatchBgService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        tokio::select! {
            _ = self.manager.watch_loop() => {},
            _ = shutdown.changed() => {
                log::info!("[etcd] shutting down watch loop");
            }
        }
    }
}
