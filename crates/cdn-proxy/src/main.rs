#[cfg(all(not(windows), not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

use arc_swap::ArcSwap;
use cdn_cache::oss::OssClient;
use cdn_cache::storage::CacheStorage;
use cdn_config::{load_cdn_config, BootstrapConfig, LiveConfig, NodeConfig};
use cdn_middleware::cc::CcEngine;
use cdn_middleware::waf::WafEngine;
use cdn_proxy::admin::purge::PurgeTaskTracker;
use cdn_proxy::admin::AdminState;
use cdn_proxy::balancer::DynamicBalancer;
use cdn_proxy::dns::DnsResolver;
use cdn_proxy::health::HealthChecker;
use cdn_proxy::health_probe::ActiveHealthCheckService;
use cdn_proxy::logging::queue::init_log_queue;
use cdn_proxy::proxy::CdnProxy;
use cdn_proxy::ssl::challenge::ChallengeStore;
use cdn_proxy::ssl::acme::AcmeClient;
use cdn_proxy::ssl::manager::CertManager;
use cdn_proxy::ssl::renewal::RenewalManager;
use cdn_proxy::ssl::storage::CertStorage;
use cdn_proxy::ssl::tls_accept::CdnTlsAccept;
use cdn_proxy::utils::redis_pool::RedisPool;
use clap::Parser;
use pingora::prelude::*;
use pingora::services::listening::Service as ListeningService;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "cdn-proxy", about = "Nozdormu CDN reverse proxy")]
struct CdnOpt {
    #[command(flatten)]
    pingora: Opt,

    /// Node identifier
    #[arg(long, default_value = "dev-node-01")]
    node_id: String,

    /// Comma-separated node labels (e.g. region:asia,dc:tokyo)
    #[arg(long, default_value = "")]
    node_labels: String,

    /// Environment name (development, staging, production)
    #[arg(long = "env", default_value = "development")]
    cdn_env: String,

    /// Comma-separated etcd endpoints
    #[arg(long, default_value = "http://127.0.0.1:2379")]
    etcd_endpoints: String,

    /// etcd key prefix
    #[arg(long, default_value = "/nozdormu")]
    etcd_prefix: String,

    /// etcd authentication username
    #[arg(long)]
    etcd_username: Option<String>,

    /// etcd authentication password
    #[arg(long)]
    etcd_password: Option<String>,

    /// etcd connection timeout in milliseconds
    #[arg(long, default_value = "5000")]
    etcd_connect_timeout: u64,

    /// Path to TLS certificate directory
    #[arg(long, default_value = "/etc/nozdormu/certs")]
    cert_path: String,

    /// Path to GeoIP database directory
    #[arg(long, default_value = "/etc/nozdormu/geoip")]
    geoip_path: String,

    /// Path to log directory
    #[arg(long, default_value = "/var/log/nozdormu")]
    log_path: String,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() {
    // ── 0. Parse CLI arguments ──
    let cdn_opt = CdnOpt::parse();

    // Initialize logger (RUST_LOG env var still controls env_logger)
    std::env::set_var("RUST_LOG", &cdn_opt.log_level);
    env_logger::init();

    // ── 1. Bootstrap: build config from CLI args ──
    let bootstrap = BootstrapConfig::from_cli(
        cdn_config::node_config::NodeInfo::from_cli(
            cdn_opt.node_id.clone(),
            cdn_opt.node_labels.clone(),
            cdn_opt.cdn_env.clone(),
        ),
        cdn_config::node_config::EtcdConfig::from_cli(
            cdn_opt.etcd_endpoints.clone(),
            cdn_opt.etcd_prefix.clone(),
            cdn_opt.etcd_username.clone(),
            cdn_opt.etcd_password.clone(),
            cdn_opt.etcd_connect_timeout,
        ),
        cdn_config::node_config::PathsConfig::from_cli(
            cdn_opt.cert_path.clone(),
            cdn_opt.geoip_path.clone(),
            cdn_opt.log_path.clone(),
        ),
        cdn_opt.log_level.clone(),
    );
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

    let node_config = NodeConfig::from_etcd_and_cli(&global_config, &bootstrap);
    if let Err(errors) = node_config.validate() {
        for e in &errors {
            log::error!("[Config] {}", e);
        }
        log::warn!("[Config] validation failed, continuing with defaults");
    }
    node_config.print_summary();

    // ── 3. Pingora server bootstrap ──
    let config_path = cdn_opt
        .pingora
        .conf
        .clone()
        .unwrap_or_else(|| "config/default.yaml".to_string());
    let cdn_config = load_cdn_config(Path::new(&config_path)).expect("failed to load CDN config");

    let mut server = Server::new(Some(cdn_opt.pingora)).expect("failed to create server");
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
        Err(e) => log::warn!(
            "[Init] etcd initial load failed: {}, starting with empty config",
            e
        ),
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
        redis_ops.clone(),
    ));

    // Build OssClient + CacheStorage for cache purge API
    let oss_client: Option<Arc<OssClient>> = match (
        &node_config.cache_oss.endpoint,
        &node_config.cache_oss.bucket,
        &node_config.cache_oss.access_key_id,
        &node_config.cache_oss.secret_access_key,
    ) {
        (Some(endpoint), Some(bucket), Some(ak), Some(sk))
            if !endpoint.is_empty() && !bucket.is_empty() =>
        {
            log::info!("[Init] OSS: {} / {}", endpoint, bucket);
            Some(Arc::new(OssClient::new(
                endpoint,
                bucket,
                &node_config.cache_oss.region,
                ak,
                sk,
                node_config.cache_oss.use_ssl,
                node_config.cache_oss.path_style,
            )))
        }
        _ => {
            log::info!("[Init] OSS not configured, cache storage disabled");
            None
        }
    };
    let cache_storage = Arc::new(CacheStorage::new(oss_client, redis_ops));
    let purge_tracker = Arc::new(PurgeTaskTracker::new());

    let health_checker = Arc::new(HealthChecker::new(
        node_config.balancer.unhealthy_threshold,
        node_config.balancer.healthy_threshold,
    ));
    let dns_resolver = Arc::new(DnsResolver::new());
    let dynamic_balancer = Arc::new(DynamicBalancer::new(
        Arc::clone(&health_checker),
        Arc::clone(&dns_resolver),
    ));

    let challenge_store = Arc::new(ChallengeStore::new());

    // ── Admin API state ──
    let admin_state = Arc::new(AdminState {
        live_config: Arc::clone(&live_config),
        balancer: Arc::clone(&dynamic_balancer),
        cc_engine: Arc::clone(&cc_engine),
        challenge_store: Arc::clone(&challenge_store),
        etcd_manager: Arc::clone(&etcd_manager),
        admin_token: node_config.security.admin_token.clone(),
        cache_storage: Arc::clone(&cache_storage),
        redis_pool: Arc::clone(&redis_pool),
        purge_tracker: Arc::clone(&purge_tracker),
        warm_tracker: Arc::new(cdn_proxy::admin::warm::WarmTaskTracker::new()),
    });

    // Active health check probes (clone refs before CdnProxy takes ownership)
    let health_probe_bg = {
        let service = ActiveHealthCheckService::new(
            Arc::clone(&live_config),
            health_checker,
            dns_resolver,
            node_config.balancer.health_check_interval,
            node_config.balancer.health_check_timeout,
            node_config.balancer.healthy_threshold,
            node_config.balancer.unhealthy_threshold,
        );
        background_service("active health check", service)
    };

    // Prefetch worker for streaming segment pre-fetching
    let prefetch_worker = Arc::new(cdn_streaming::prefetch::PrefetchWorker::new(Arc::clone(
        &cache_storage,
    )));

    // ── Certificate storage (shared between TLS listener and renewal) ──
    let cert_storage = Arc::new(CertStorage::new(Path::new(&node_config.paths.certs)));
    cert_storage.load_all();
    let cert_manager = Arc::new(CertManager::new(Arc::clone(&cert_storage)));

    // ── ACME client and renewal manager ──
    let acme_client = Arc::new(AcmeClient::from_config(
        &node_config.ssl.acme_providers,
        node_config.ssl.acme_environment == "staging",
        node_config.ssl.acme_email.clone(),
        &node_config.ssl.eab_credentials,
        Arc::clone(&challenge_store),
        Arc::clone(&redis_pool),
    ));

    let renewal_manager = Arc::new(RenewalManager::new(
        Arc::clone(&cert_storage),
        Arc::clone(&cert_manager),
        Arc::clone(&acme_client),
        Arc::clone(&redis_pool),
        node_config.node.id.clone(),
        node_config.ssl.renewal_days,
    ));

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
        default_image_optimization: node_config.image_optimization.clone(),
        prefetch_worker,
        node_id: Arc::from(node_config.node.id.as_str()),
        admin_state: Arc::clone(&admin_state),
        cache_storage: Arc::clone(&cache_storage),
        coalescing_map: Arc::new(dashmap::DashMap::new()),
    };

    let mut proxy_service = http_proxy_service(&server.configuration, cdn_proxy);
    proxy_service.add_tcp(&cdn_config.listen);
    log::info!("proxy listening on {}", cdn_config.listen);

    // ── TLS listener (optional) ──
    if let Some(ref tls_addr) = cdn_config.tls_listen {
        let tls_accept = CdnTlsAccept::new(Arc::clone(&cert_manager));
        let callbacks: pingora::listeners::TlsAcceptCallbacks = Box::new(tls_accept);
        let mut tls_settings =
            pingora::listeners::tls::TlsSettings::with_callbacks(callbacks)
                .expect("failed to create TLS settings");
        tls_settings.enable_h2();

        if cdn_config.early_data {
            tls_settings
                .set_max_early_data(cdn_config.max_early_data)
                .expect("failed to set max_early_data");
            log::info!(
                "[TLS] 0-RTT early data enabled, max_early_data={}",
                cdn_config.max_early_data
            );
        }

        proxy_service.add_tls_with_settings(tls_addr, None, tls_settings);
        log::info!("proxy TLS listening on {}", tls_addr);
    }

    // ── 7. Prometheus metrics ──
    let mut prometheus_service = ListeningService::prometheus_http_service();
    prometheus_service.add_tcp(&cdn_config.metrics_listen);
    log::info!("metrics listening on {}", cdn_config.metrics_listen);

    // ── 8. Background services ──

    // etcd watch loop for live config updates
    let etcd_bg = {
        let mgr = Arc::clone(&etcd_manager);
        background_service("etcd watch", EtcdWatchBgService { manager: mgr })
    };

    // Certificate auto-renewal
    let renewal_bg = background_service(
        "certificate renewal",
        RenewalBgService {
            manager: Arc::clone(&renewal_manager),
        },
    );

    // ── 9. Register and run ──
    server.add_service(background);
    server.add_service(proxy_service);
    server.add_service(prometheus_service);
    server.add_service(etcd_bg);
    server.add_service(health_probe_bg);
    server.add_service(renewal_bg);

    log::info!("Nozdormu CDN starting...");
    server.run_forever();
}

// ── Background service wrappers ──

use async_trait::async_trait;
use pingora::server::ShutdownWatch;
use pingora::services::background::BackgroundService;

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

struct RenewalBgService {
    manager: Arc<RenewalManager>,
}

#[async_trait]
impl BackgroundService for RenewalBgService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        log::info!("[Renewal] certificate renewal service started");

        // First check: 60 seconds after startup
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {},
            _ = shutdown.changed() => {
                log::info!("[Renewal] shutting down before first check");
                return;
            }
        }

        loop {
            let renewed = self.manager.check_and_renew().await;
            log::info!("[Renewal] scan complete, renewed={}", renewed);

            // Next check: every 24 hours
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(86400)) => {},
                _ = shutdown.changed() => {
                    log::info!("[Renewal] shutting down");
                    return;
                }
            }
        }
    }
}
