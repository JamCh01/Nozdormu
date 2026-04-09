use arc_swap::ArcSwap;
use async_trait::async_trait;
use cdn_common::{HealthCheckType, OriginProtocol};
use cdn_config::LiveConfig;
use pingora::server::ShutdownWatch;
use pingora::services::background::BackgroundService;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

use crate::dns::DnsResolver;
use crate::health::HealthChecker;
use crate::logging::metrics::{HEALTH_CHECK_DURATION, HEALTH_CHECK_TOTAL};
use crate::utils::rand as fastrand;

// ── Probe target (resolved from config) ──

#[derive(Debug, Clone)]
struct ProbeTarget {
    site_id: String,
    origin_id: String,
    host: String,
    port: u16,
    protocol: OriginProtocol,
    sni: Option<String>,
    verify_ssl: bool,
    check_type: HealthCheckType,
    path: String,
    interval: Duration,
    timeout: Duration,
    healthy_threshold: u32,
    unhealthy_threshold: u32,
    expected_codes: Option<Vec<u16>>,
    host_header: Option<String>,
}

/// Unique key for a (site, origin) pair.
fn probe_key(site_id: &str, origin_id: &str) -> String {
    format!("{}\0{}", site_id, origin_id)
}

// ── Per-origin probe state ──

struct ProbeState {
    consecutive_successes: u32,
    consecutive_failures: u32,
    currently_healthy: bool,
}

impl ProbeState {
    fn new() -> Self {
        Self {
            consecutive_successes: 0,
            consecutive_failures: 0,
            currently_healthy: true,
        }
    }
}

// ── Active health check background service ──

pub struct ActiveHealthCheckService {
    live_config: Arc<ArcSwap<LiveConfig>>,
    health_checker: Arc<HealthChecker>,
    dns: Arc<DnsResolver>,
    global_interval: u64,
    global_timeout: u64,
    global_healthy_threshold: u32,
    global_unhealthy_threshold: u32,
}

impl ActiveHealthCheckService {
    pub fn new(
        live_config: Arc<ArcSwap<LiveConfig>>,
        health_checker: Arc<HealthChecker>,
        dns: Arc<DnsResolver>,
        global_interval: u64,
        global_timeout: u64,
        global_healthy_threshold: u32,
        global_unhealthy_threshold: u32,
    ) -> Self {
        Self {
            live_config,
            health_checker,
            dns,
            global_interval,
            global_timeout,
            global_healthy_threshold,
            global_unhealthy_threshold,
        }
    }

    /// Compute the desired set of probe targets from current config.
    fn compute_desired_probes(&self) -> HashMap<String, ProbeTarget> {
        let config = self.live_config.load();
        let mut targets = HashMap::new();

        for (site_id, site) in &config.sites {
            if !site.enabled {
                continue;
            }
            let hc = &site.load_balancer.health_check;
            if !hc.enabled {
                continue;
            }

            for origin in &site.origins {
                if !origin.enabled {
                    continue;
                }

                let key = probe_key(site_id, &origin.id);
                let interval = if hc.interval > 0 {
                    hc.interval
                } else {
                    self.global_interval
                };
                let timeout = if hc.timeout > 0 {
                    hc.timeout
                } else {
                    self.global_timeout
                };

                targets.insert(
                    key,
                    ProbeTarget {
                        site_id: site_id.clone(),
                        origin_id: origin.id.clone(),
                        host: origin.host.clone(),
                        port: origin.port,
                        protocol: origin.protocol.clone(),
                        sni: origin.sni.clone(),
                        verify_ssl: origin.verify_ssl,
                        check_type: hc.r#type.clone(),
                        path: hc.path.clone(),
                        interval: Duration::from_secs(interval),
                        timeout: Duration::from_secs(timeout),
                        healthy_threshold: if hc.healthy_threshold > 0 {
                            hc.healthy_threshold
                        } else {
                            self.global_healthy_threshold
                        },
                        unhealthy_threshold: if hc.unhealthy_threshold > 0 {
                            hc.unhealthy_threshold
                        } else {
                            self.global_unhealthy_threshold
                        },
                        expected_codes: hc.expected_codes.clone(),
                        host_header: hc.host_header.clone(),
                    },
                );
            }
        }

        targets
    }
}

#[async_trait]
impl BackgroundService for ActiveHealthCheckService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        log::info!("[HealthProbe] active health check service started");

        let mut running: HashMap<String, JoinHandle<()>> = HashMap::new();

        loop {
            // Reconcile every 5 seconds
            tokio::select! {
                _ = shutdown.changed() => {
                    log::info!("[HealthProbe] shutting down, aborting {} probe tasks", running.len());
                    for (_, handle) in running.drain() {
                        handle.abort();
                    }
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }

            let desired = self.compute_desired_probes();

            // Remove tasks for origins no longer in desired set
            running.retain(|key, handle| {
                if !desired.contains_key(key) {
                    log::info!("[HealthProbe] stopping probe for {}", key.replace('\0', "/"));
                    handle.abort();
                    false
                } else {
                    // Also remove finished/panicked tasks so they get re-spawned
                    !handle.is_finished()
                }
            });

            // Spawn tasks for new origins
            for (key, target) in &desired {
                if running.contains_key(key) {
                    continue;
                }

                log::info!(
                    "[HealthProbe] starting probe: site={} origin={} type={:?} interval={}s",
                    target.site_id,
                    target.origin_id,
                    target.check_type,
                    target.interval.as_secs()
                );

                let health = Arc::clone(&self.health_checker);
                let dns = Arc::clone(&self.dns);
                let target = target.clone();

                let handle = tokio::spawn(async move {
                    probe_loop(target, health, dns).await;
                });

                running.insert(key.clone(), handle);
            }
        }

        log::info!("[HealthProbe] active health check service stopped");
    }
}

// ── Per-origin probe loop ──

async fn probe_loop(
    target: ProbeTarget,
    health: Arc<HealthChecker>,
    dns: Arc<DnsResolver>,
) {
    // Initial jitter: random delay in [0, interval) to spread probes
    let jitter_ms = fastrand::u64(..target.interval.as_millis() as u64);
    tokio::time::sleep(Duration::from_millis(jitter_ms)).await;

    // Pre-build HTTP client if needed (reuse across probes)
    let http_client = if matches!(target.check_type, HealthCheckType::Http) {
        Some(build_http_client(&target))
    } else {
        None
    };

    let mut state = ProbeState::new();
    let mut interval = tokio::time::interval(target.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the first immediate tick (we already waited via jitter)
    interval.tick().await;

    loop {
        interval.tick().await;

        let start = Instant::now();
        let result = match target.check_type {
            HealthCheckType::Http => {
                http_probe(&target, &dns, http_client.as_ref().unwrap()).await
            }
            HealthCheckType::Tcp => tcp_probe(&target, &dns).await,
        };
        let elapsed = start.elapsed();

        // Record Prometheus metrics
        let result_label = if result.is_ok() { "success" } else { "failure" };
        HEALTH_CHECK_TOTAL
            .with_label_values(&[
                target.site_id.as_str(),
                target.origin_id.as_str(),
                result_label,
            ])
            .inc();
        HEALTH_CHECK_DURATION
            .with_label_values(&[target.site_id.as_str(), target.origin_id.as_str()])
            .observe(elapsed.as_secs_f64());

        // Update probe state and check thresholds
        let success = result.is_ok();
        update_probe_state(&target, &health, &mut state, success);

        if let Err(ref reason) = result {
            log::debug!(
                "[HealthProbe] probe failed: site={} origin={} reason={}",
                target.site_id, target.origin_id, reason
            );
        }
    }
}

// ── Probe state management ──

fn update_probe_state(
    target: &ProbeTarget,
    health: &HealthChecker,
    state: &mut ProbeState,
    success: bool,
) {
    // Record metadata for admin API
    health.record_active_check(&target.site_id, &target.origin_id, success);

    if success {
        state.consecutive_failures = 0;
        state.consecutive_successes += 1;

        if !state.currently_healthy
            && state.consecutive_successes >= target.healthy_threshold
        {
            state.currently_healthy = true;
            health.set_status(&target.site_id, &target.origin_id, true);
            log::info!(
                "[HealthProbe] origin recovered: site={} origin={} after {} successes",
                target.site_id, target.origin_id, state.consecutive_successes
            );
        }
    } else {
        state.consecutive_successes = 0;
        state.consecutive_failures += 1;

        if state.currently_healthy
            && state.consecutive_failures >= target.unhealthy_threshold
        {
            state.currently_healthy = false;
            health.set_status(&target.site_id, &target.origin_id, false);
            log::warn!(
                "[HealthProbe] origin marked unhealthy: site={} origin={} after {} failures",
                target.site_id, target.origin_id, state.consecutive_failures
            );
        }
    }
}

// ── HTTP probe ──

fn build_http_client(target: &ProbeTarget) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(target.timeout)
        .connect_timeout(target.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .user_agent("Nozdormu-HealthCheck/1.0");

    if target.protocol == OriginProtocol::Https {
        builder = builder.danger_accept_invalid_certs(!target.verify_ssl);
    }

    builder.build().unwrap_or_else(|e| {
        log::error!(
            "[HealthProbe] failed to build HTTP client for {}:{}: {}",
            target.host, target.port, e
        );
        reqwest::Client::new()
    })
}

async fn http_probe(
    target: &ProbeTarget,
    dns: &DnsResolver,
    client: &reqwest::Client,
) -> Result<(), String> {
    let addr = resolve_target(target, dns).await?;

    let scheme = match target.protocol {
        OriginProtocol::Https => "https",
        OriginProtocol::Http => "http",
    };

    let host_header = target
        .host_header
        .as_deref()
        .unwrap_or(&target.host);

    // For HTTPS, use the SNI hostname in the URL so TLS handshake sends correct SNI.
    // For HTTP, connect directly to the resolved IP.
    let url = if target.protocol == OriginProtocol::Https {
        let sni_host = target.sni.as_deref().unwrap_or(&target.host);
        format!("{}://{}:{}{}", scheme, sni_host, addr.port(), target.path)
    } else {
        format!("{}://{}:{}{}", scheme, addr.ip(), addr.port(), target.path)
    };

    let mut req = client.get(&url).header("Host", host_header);

    // For HTTPS, resolve the SNI hostname to the actual IP
    if target.protocol == OriginProtocol::Https {
        let sni_host = target.sni.as_deref().unwrap_or(&target.host);
        // reqwest resolves the hostname in the URL; we need to override DNS
        // by building a new client with resolve(). Since we pre-build the client,
        // we use a workaround: connect to IP directly with a custom header.
        // Actually, reqwest's resolve() is per-client-builder, not per-request.
        // So we connect to the SNI hostname and let reqwest resolve it.
        // The DNS resolver cache will handle this efficiently.
        let _ = sni_host; // URL already uses sni_host
        let _ = req;
        req = client.get(&url).header("Host", host_header);
    }

    let resp = req.send().await.map_err(|e| format!("HTTP request failed: {}", e))?;

    let status = resp.status().as_u16();
    if is_expected_status(status, &target.expected_codes) {
        Ok(())
    } else {
        Err(format!("unexpected status: {}", status))
    }
}

fn is_expected_status(status: u16, expected_codes: &Option<Vec<u16>>) -> bool {
    match expected_codes {
        Some(codes) if !codes.is_empty() => codes.contains(&status),
        _ => (200..300).contains(&status),
    }
}

// ── TCP probe ──

async fn tcp_probe(
    target: &ProbeTarget,
    dns: &DnsResolver,
) -> Result<(), String> {
    let addr = resolve_target(target, dns).await?;

    match tokio::time::timeout(
        target.timeout,
        tokio::net::TcpStream::connect(addr),
    )
    .await
    {
        Ok(Ok(_stream)) => Ok(()),
        Ok(Err(e)) => Err(format!("TCP connect failed: {}", e)),
        Err(_) => Err("TCP connect timeout".to_string()),
    }
}

// ── DNS resolution helper ──

async fn resolve_target(
    target: &ProbeTarget,
    dns: &DnsResolver,
) -> Result<SocketAddr, String> {
    dns.resolve_to_socket(&target.host, target.port)
        .await
        .ok_or_else(|| format!("DNS resolution failed for {}:{}", target.host, target.port))
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_expected_status_default() {
        // None = accept 200-299
        assert!(is_expected_status(200, &None));
        assert!(is_expected_status(204, &None));
        assert!(is_expected_status(299, &None));
        assert!(!is_expected_status(301, &None));
        assert!(!is_expected_status(404, &None));
        assert!(!is_expected_status(500, &None));
    }

    #[test]
    fn test_is_expected_status_custom() {
        let codes = Some(vec![200, 204, 301]);
        assert!(is_expected_status(200, &codes));
        assert!(is_expected_status(204, &codes));
        assert!(is_expected_status(301, &codes));
        assert!(!is_expected_status(302, &codes));
        assert!(!is_expected_status(500, &codes));
    }

    #[test]
    fn test_is_expected_status_empty_vec() {
        // Empty vec falls back to 200-299
        let codes = Some(vec![]);
        assert!(is_expected_status(200, &codes));
        assert!(!is_expected_status(404, &codes));
    }

    #[test]
    fn test_probe_key() {
        assert_eq!(probe_key("site1", "origin1"), "site1\0origin1");
        assert_ne!(
            probe_key("site1", "origin1"),
            probe_key("site1", "origin2")
        );
    }

    #[test]
    fn test_probe_state_threshold_healthy_to_unhealthy() {
        let health = Arc::new(HealthChecker::new(3, 2));
        let target = make_test_target(3, 2);
        let mut state = ProbeState::new();

        // 2 failures — still healthy
        update_probe_state(&target, &health, &mut state, false);
        update_probe_state(&target, &health, &mut state, false);
        assert!(state.currently_healthy);
        assert!(health.is_healthy("test-site", "test-origin"));

        // 3rd failure — crosses threshold
        update_probe_state(&target, &health, &mut state, false);
        assert!(!state.currently_healthy);
        assert!(!health.is_healthy("test-site", "test-origin"));
    }

    #[test]
    fn test_probe_state_threshold_unhealthy_to_healthy() {
        let health = Arc::new(HealthChecker::new(3, 2));
        let target = make_test_target(3, 2);
        let mut state = ProbeState::new();

        // Mark unhealthy
        for _ in 0..3 {
            update_probe_state(&target, &health, &mut state, false);
        }
        assert!(!state.currently_healthy);

        // 1 success — still unhealthy
        update_probe_state(&target, &health, &mut state, true);
        assert!(!state.currently_healthy);

        // 2nd success — recovers
        update_probe_state(&target, &health, &mut state, true);
        assert!(state.currently_healthy);
        assert!(health.is_healthy("test-site", "test-origin"));
    }

    #[test]
    fn test_probe_state_failure_resets_success_counter() {
        let health = Arc::new(HealthChecker::new(3, 2));
        let target = make_test_target(3, 2);
        let mut state = ProbeState::new();

        // Mark unhealthy
        for _ in 0..3 {
            update_probe_state(&target, &health, &mut state, false);
        }

        // 1 success, then failure resets
        update_probe_state(&target, &health, &mut state, true);
        update_probe_state(&target, &health, &mut state, false);
        assert_eq!(state.consecutive_successes, 0);
        assert_eq!(state.consecutive_failures, 1);

        // 1 more success — not enough (need 2)
        update_probe_state(&target, &health, &mut state, true);
        assert!(!state.currently_healthy);
    }

    #[test]
    fn test_probe_state_records_active_check() {
        let health = Arc::new(HealthChecker::new(3, 2));
        let target = make_test_target(3, 2);
        let mut state = ProbeState::new();

        update_probe_state(&target, &health, &mut state, true);
        let detail = health.get_detail("test-site", "test-origin");
        assert!(detail.last_active_check.is_some());
        assert_eq!(detail.last_active_success, Some(true));

        update_probe_state(&target, &health, &mut state, false);
        let detail = health.get_detail("test-site", "test-origin");
        assert_eq!(detail.last_active_success, Some(false));
    }

    fn make_test_target(unhealthy_threshold: u32, healthy_threshold: u32) -> ProbeTarget {
        ProbeTarget {
            site_id: "test-site".to_string(),
            origin_id: "test-origin".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            protocol: OriginProtocol::Http,
            sni: None,
            verify_ssl: false,
            check_type: HealthCheckType::Http,
            path: "/health".to_string(),
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(5),
            healthy_threshold,
            unhealthy_threshold,
            expected_codes: None,
            host_header: None,
        }
    }
}
