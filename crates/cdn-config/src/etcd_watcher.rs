use crate::global_config::GlobalConfig;
use crate::live_config::{filter_sites_by_labels, LiveConfig};
use crate::node_config::EtcdConfig;
use arc_swap::ArcSwap;
use cdn_common::SiteConfig;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Manages loading and watching site configurations from etcd.
pub struct EtcdConfigManager {
    config: EtcdConfig,
    node_labels: HashSet<String>,
    live: Arc<ArcSwap<LiveConfig>>,
    last_revision: AtomicI64,
}

impl EtcdConfigManager {
    pub fn new(
        config: EtcdConfig,
        node_labels: HashSet<String>,
        live: Arc<ArcSwap<LiveConfig>>,
    ) -> Self {
        Self {
            config,
            node_labels,
            live,
            last_revision: AtomicI64::new(0),
        }
    }

    /// Returns a handle to the live config for use in the proxy.
    pub fn live_config(&self) -> Arc<ArcSwap<LiveConfig>> {
        Arc::clone(&self.live)
    }

    /// Full load: GET all sites from etcd, filter by labels, build indexes.
    pub async fn load_all(&self) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        let endpoints: Vec<&str> = self.config.endpoints.iter().map(|s| s.as_str()).collect();
        let mut client = etcd_client::Client::connect(&endpoints, None).await?;

        let prefix = format!("{}/sites/", self.config.prefix);
        let opts = etcd_client::GetOptions::new().with_prefix();
        let resp = client.get(prefix.as_bytes(), Some(opts)).await?;

        let revision = resp
            .header()
            .map(|h| h.revision())
            .unwrap_or(0);
        self.last_revision.fetch_max(revision, Ordering::SeqCst);

        let mut sites: HashMap<String, Arc<SiteConfig>> = HashMap::new();

        for kv in resp.kvs() {
            let key = match String::from_utf8(kv.key().to_vec()) {
                Ok(k) => k,
                Err(e) => {
                    log::warn!("[etcd] skipping key with invalid UTF-8: {}", e);
                    continue;
                }
            };
            let site_id = key
                .strip_prefix(&prefix)
                .unwrap_or("")
                .to_string();

            if site_id.is_empty() {
                continue;
            }

            match serde_json::from_slice::<SiteConfig>(kv.value()) {
                Ok(site) => {
                    site.warn_invalid();
                    sites.insert(site_id, Arc::new(site));
                }
                Err(e) => {
                    log::warn!("failed to parse site config for '{}': {}", site_id, e);
                }
            }
        }

        // Filter by node labels
        let filtered = filter_sites_by_labels(sites, &self.node_labels);

        let mut live = LiveConfig {
            sites: filtered,
            ..Default::default()
        };
        live.build_indexes();

        let (site_count, domain_count) = live.stats();
        log::info!(
            "loaded {} sites ({} domains) from etcd at revision {}",
            site_count,
            domain_count,
            revision
        );

        self.live.store(Arc::new(live));
        Ok(revision)
    }

    /// Watch for incremental changes from etcd.
    /// Runs indefinitely, reconnecting with exponential backoff on failure.
    pub async fn watch_loop(&self) {
        let mut backoff_secs: u64 = 5;
        loop {
            match self.watch_once().await {
                Ok(()) => {
                    // Stream ended normally (e.g., canceled) — reset backoff
                    backoff_secs = 5;
                }
                Err(e) => {
                    log::error!("etcd watch error: {}, reconnecting in {}s...", e, backoff_secs);
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                }
            }
        }
    }

    async fn watch_once(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let endpoints: Vec<&str> = self.config.endpoints.iter().map(|s| s.as_str()).collect();
        let mut client = etcd_client::Client::connect(&endpoints, None).await?;

        let prefix = format!("{}/sites/", self.config.prefix);
        let start_rev = self.last_revision.load(Ordering::SeqCst) + 1;

        let opts = etcd_client::WatchOptions::new()
            .with_prefix()
            .with_start_revision(start_rev);

        let (_watcher, mut stream) = client.watch(prefix.as_bytes(), Some(opts)).await?;
        log::info!("etcd watch started from revision {}", start_rev);

        while let Some(resp) = stream.message().await? {
            if resp.canceled() {
                log::warn!("etcd watch canceled: {:?}", resp.cancel_reason());
                break;
            }

            // Only clone the sites map (Arc values are cheap).
            // domain_index and wildcard_index will be rebuilt by build_indexes().
            let current = self.live.load();
            let mut sites = current.sites.clone();
            let mut changed = false;
            let mut max_revision: i64 = 0;

            for event in resp.events() {
                let kv = match event.kv() {
                    Some(kv) => kv,
                    None => continue,
                };

                let key = match String::from_utf8(kv.key().to_vec()) {
                    Ok(k) => k,
                    Err(e) => {
                        log::warn!("[etcd] skipping watch event with invalid UTF-8 key: {}", e);
                        continue;
                    }
                };
                let site_id = key
                    .strip_prefix(&prefix)
                    .unwrap_or("")
                    .to_string();

                if site_id.is_empty() {
                    continue;
                }

                let rev = kv.mod_revision();
                if rev > max_revision {
                    max_revision = rev;
                }

                match event.event_type() {
                    etcd_client::EventType::Put => {
                        match serde_json::from_slice::<SiteConfig>(kv.value()) {
                            Ok(site) => {
                                // Check label match
                                if !site.target_labels.is_empty()
                                    && !site
                                        .target_labels
                                        .iter()
                                        .any(|l| self.node_labels.contains(l))
                                {
                                    // Site no longer matches this node — remove if present
                                    if sites.remove(&site_id).is_some() {
                                        log::info!(
                                            "[rev={}] site '{}' labels {:?} no longer match node, removing",
                                            rev, site_id, site.target_labels
                                        );
                                        changed = true;
                                    } else {
                                        log::debug!(
                                            "site '{}' labels {:?} don't match node, skipping",
                                            site_id,
                                            site.target_labels
                                        );
                                    }
                                    continue;
                                }
                                log::info!("[rev={}] site '{}' updated", rev, site_id);
                                site.warn_invalid();
                                sites.insert(site_id, Arc::new(site));
                                changed = true;
                            }
                            Err(e) => {
                                log::warn!("failed to parse site '{}': {}", site_id, e);
                            }
                        }
                    }
                    etcd_client::EventType::Delete => {
                        log::info!("[rev={}] site '{}' deleted", rev, site_id);
                        sites.remove(&site_id);
                        changed = true;
                    }
                }
            }

            if changed {
                let mut new_config = LiveConfig {
                    sites,
                    ..Default::default()
                };
                new_config.build_indexes();
                let (site_count, domain_count) = new_config.stats();
                self.live.store(Arc::new(new_config));
                log::info!("config updated: {} sites, {} domains", site_count, domain_count);
            }

            // Update revision AFTER config is stored, so a crash can't leave
            // revision ahead of applied config state.
            if max_revision > 0 {
                self.last_revision.fetch_max(max_revision, Ordering::SeqCst);
            }
        }

        Ok(())
    }
}

/// Load cluster-shared global configuration from etcd.
///
/// Reads keys under `{prefix}/global/` and deserializes each into the
/// corresponding `GlobalConfig` field. Called once at startup before
/// `EtcdConfigManager` is created.
///
/// Returns `GlobalConfig::default()` (all `None`) if etcd is unreachable
/// or no global keys exist — this preserves backward compatibility with
/// env-only deployments.
pub async fn load_global_config(etcd_config: &EtcdConfig) -> GlobalConfig {
    let endpoints: Vec<&str> = etcd_config.endpoints.iter().map(|s| s.as_str()).collect();
    let mut client = match etcd_client::Client::connect(&endpoints, None).await {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "[etcd] failed to connect for global config: {}, using env/defaults",
                e
            );
            return GlobalConfig::default();
        }
    };

    let prefix = format!("{}/global/", etcd_config.prefix);
    let opts = etcd_client::GetOptions::new().with_prefix();
    let resp = match client.get(prefix.as_bytes(), Some(opts)).await {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "[etcd] failed to load global config: {}, using env/defaults",
                e
            );
            return GlobalConfig::default();
        }
    };

    let mut global = GlobalConfig::default();

    for kv in resp.kvs() {
        let key = match String::from_utf8(kv.key().to_vec()) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let suffix = match key.strip_prefix(&prefix) {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };

        let value = kv.value();
        match suffix {
            "redis" => match serde_json::from_slice(value) {
                Ok(v) => global.redis = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/redis: {}", e),
            },
            "security" => match serde_json::from_slice(value) {
                Ok(v) => global.security = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/security: {}", e),
            },
            "balancer" => match serde_json::from_slice(value) {
                Ok(v) => global.balancer = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/balancer: {}", e),
            },
            "proxy" => match serde_json::from_slice(value) {
                Ok(v) => global.proxy = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/proxy: {}", e),
            },
            "cache" => match serde_json::from_slice(value) {
                Ok(v) => global.cache = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/cache: {}", e),
            },
            "ssl" => match serde_json::from_slice(value) {
                Ok(v) => global.ssl = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/ssl: {}", e),
            },
            "logging" => match serde_json::from_slice(value) {
                Ok(v) => global.logging = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/logging: {}", e),
            },
            "compression" => match serde_json::from_slice(value) {
                Ok(v) => global.compression = Some(v),
                Err(e) => log::warn!("[etcd] failed to parse global/compression: {}", e),
            },
            other => {
                log::debug!("[etcd] ignoring unknown global key: {}", other);
            }
        }
    }

    let loaded_count = [
        global.redis.is_some(),
        global.security.is_some(),
        global.balancer.is_some(),
        global.proxy.is_some(),
        global.cache.is_some(),
        global.ssl.is_some(),
        global.logging.is_some(),
        global.compression.is_some(),
    ]
    .iter()
    .filter(|&&x| x)
    .count();

    log::info!(
        "[etcd] loaded {}/8 global config sections from etcd",
        loaded_count
    );
    global
}
