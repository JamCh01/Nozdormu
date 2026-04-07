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
    pub async fn load_all(&self) -> Result<i64, Box<dyn std::error::Error>> {
        let endpoints: Vec<&str> = self.config.endpoints.iter().map(|s| s.as_str()).collect();
        let mut client = etcd_client::Client::connect(&endpoints, None).await?;

        let prefix = format!("{}/sites/", self.config.prefix);
        let opts = etcd_client::GetOptions::new().with_prefix();
        let resp = client.get(prefix.as_bytes(), Some(opts)).await?;

        let revision = resp
            .header()
            .map(|h| h.revision())
            .unwrap_or(0);
        self.last_revision.store(revision, Ordering::SeqCst);

        let mut sites: HashMap<String, Arc<SiteConfig>> = HashMap::new();

        for kv in resp.kvs() {
            let key = String::from_utf8_lossy(kv.key());
            let site_id = key
                .strip_prefix(&prefix)
                .unwrap_or("")
                .to_string();

            if site_id.is_empty() {
                continue;
            }

            match serde_json::from_slice::<SiteConfig>(kv.value()) {
                Ok(site) => {
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
    /// Runs indefinitely, reconnecting on failure.
    pub async fn watch_loop(&self) {
        loop {
            if let Err(e) = self.watch_once().await {
                log::error!("etcd watch error: {}, reconnecting in 5s...", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }

    async fn watch_once(&self) -> Result<(), Box<dyn std::error::Error>> {
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

            let mut changed = false;

            for event in resp.events() {
                let kv = match event.kv() {
                    Some(kv) => kv,
                    None => continue,
                };

                let key = String::from_utf8_lossy(kv.key());
                let site_id = key
                    .strip_prefix(&prefix)
                    .unwrap_or("")
                    .to_string();

                if site_id.is_empty() {
                    continue;
                }

                let rev = kv.mod_revision();
                self.last_revision.store(rev, Ordering::SeqCst);

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
                                    log::debug!(
                                        "site '{}' labels {:?} don't match node, skipping",
                                        site_id,
                                        site.target_labels
                                    );
                                    continue;
                                }
                                log::info!("[rev={}] site '{}' updated", rev, site_id);
                                let current = self.live.load();
                                let mut new_config = (**current).clone();
                                new_config.sites.insert(site_id, Arc::new(site));
                                new_config.build_indexes();
                                self.live.store(Arc::new(new_config));
                                changed = true;
                            }
                            Err(e) => {
                                log::warn!("failed to parse site '{}': {}", site_id, e);
                            }
                        }
                    }
                    etcd_client::EventType::Delete => {
                        log::info!("[rev={}] site '{}' deleted", rev, site_id);
                        let current = self.live.load();
                        let mut new_config = (**current).clone();
                        new_config.sites.remove(&site_id);
                        new_config.build_indexes();
                        self.live.store(Arc::new(new_config));
                        changed = true;
                    }
                }
            }

            if changed {
                let (sites, domains) = self.live.load().stats();
                log::info!("config updated: {} sites, {} domains", sites, domains);
            }
        }

        Ok(())
    }
}
