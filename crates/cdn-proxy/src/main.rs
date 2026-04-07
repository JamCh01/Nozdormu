#[cfg(all(not(windows), not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

mod proxy;
mod context;
mod protocol;
mod balancer;
mod health;
mod dns;
mod ssl;
mod logging;
mod admin;
mod utils;

use arc_swap::ArcSwap;
use cdn_config::{load_cdn_config, LiveConfig, NodeConfig};
use pingora::prelude::*;
use pingora::services::listening::Service as ListeningService;
use proxy::CdnProxy;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    env_logger::init();

    // ── 1. Load node config from environment ──
    let node_config = NodeConfig::from_env();
    if let Err(errors) = node_config.validate() {
        for e in &errors {
            log::error!("[Config] {}", e);
        }
        log::warn!("[Config] validation failed, continuing with defaults");
    }
    node_config.print_summary();

    // ── 2. Pingora server bootstrap ──
    let opt = Opt::parse_args();
    let config_path = opt
        .conf
        .clone()
        .unwrap_or_else(|| "config/default.yaml".to_string());
    let cdn_config =
        load_cdn_config(Path::new(&config_path)).expect("failed to load CDN config");

    let mut server = Server::new(Some(opt)).expect("failed to create server");
    server.bootstrap();

    // ── 3. LiveConfig (will be populated from etcd) ──
    let live_config = Arc::new(ArcSwap::from_pointee(LiveConfig::default()));

    // TODO: Phase 1 integration — connect to etcd, load_all, spawn watch_loop
    // For now, LiveConfig starts empty. Sites will be loaded from etcd when available.

    // ── 4. Static upstream (temporary, replaced by dynamic routing in Phase 7) ──
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

    // ── 5. Create proxy service ──
    let cdn_proxy = CdnProxy {
        lb,
        sni,
        tls: has_tls,
        live_config,
    };

    let mut proxy_service = http_proxy_service(&server.configuration, cdn_proxy);
    proxy_service.add_tcp(&cdn_config.listen);
    log::info!("proxy listening on {}", cdn_config.listen);

    // ── 6. Prometheus metrics ──
    let mut prometheus_service = ListeningService::prometheus_http_service();
    prometheus_service.add_tcp(&cdn_config.metrics_listen);
    log::info!("metrics listening on {}", cdn_config.metrics_listen);

    // ── 7. Register and run ──
    server.add_service(background);
    server.add_service(proxy_service);
    server.add_service(prometheus_service);

    log::info!("Nozdormu CDN starting...");
    server.run_forever();
}
