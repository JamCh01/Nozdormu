//! Background prefetch worker — fetches video segments from origin into cache.
//!
//! Fire-and-forget via `tokio::spawn`. Per-site concurrency via `Semaphore`.
//! Deduplication via `in_flight` DashMap.

use cdn_cache::key::generate_cache_key;
use cdn_cache::storage::{self, CacheStorage};
use cdn_common::{OriginConfig, SiteConfig};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use prometheus::{register_histogram_vec, register_int_counter_vec, HistogramVec, IntCounterVec};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;

// ── Prometheus metrics ──

pub static PREFETCH_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_streaming_prefetch_total",
        "Total prefetch requests",
        &["site_id", "result"]
    )
    .unwrap()
});

pub static PREFETCH_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_streaming_prefetch_duration_seconds",
        "Prefetch request duration",
        &["site_id"],
        vec![0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
    .unwrap()
});

/// Background prefetch worker.
///
/// Fetches video segments from origin directly via reqwest and stores them
/// in the CDN cache. Operates via fire-and-forget `tokio::spawn` calls
/// triggered from the response path when a manifest is detected.
pub struct PrefetchWorker {
    cache_storage: Arc<CacheStorage>,
    http_client: reqwest::Client,
    /// Per-site concurrency semaphore
    site_semaphores: DashMap<String, Arc<Semaphore>>,
    /// Track in-flight prefetches to avoid duplicates (key = cache_key)
    in_flight: DashMap<String, ()>,
}

impl PrefetchWorker {
    pub fn new(cache_storage: Arc<CacheStorage>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(5))
            .pool_max_idle_per_host(10)
            .build()
            .unwrap_or_default();

        Self {
            cache_storage,
            http_client,
            site_semaphores: DashMap::new(),
            in_flight: DashMap::new(),
        }
    }

    /// Fire-and-forget: prefetch the next N segments from origin.
    ///
    /// Called from `response_body_filter` when a manifest is detected.
    /// Spawns async tasks that fetch segments and store them in cache.
    pub fn prefetch_segments(
        self: &Arc<Self>,
        site_id: String,
        site_config: Arc<SiteConfig>,
        origin: OriginConfig,
        segment_urls: Vec<String>,
        host: String,
    ) {
        let worker = Arc::clone(self);
        let prefetch_count = site_config.streaming.prefetch.prefetch_count as usize;
        let concurrency = site_config.streaming.prefetch.concurrency_limit as usize;

        tokio::spawn(async move {
            let semaphore = worker
                .site_semaphores
                .entry(site_id.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(concurrency)))
                .clone();

            let urls_to_fetch: Vec<String> = segment_urls
                .into_iter()
                .take(prefetch_count)
                .collect();

            for url in urls_to_fetch {
                // Generate cache key for this segment
                let (seg_host, seg_path) = parse_url_components(&url, &host);
                let cache_key = generate_cache_key(
                    &site_id,
                    &seg_host,
                    &seg_path,
                    None,
                    false,
                    &[],
                );

                // Deduplication: skip if already in-flight
                if worker.in_flight.contains_key(&cache_key) {
                    continue;
                }
                worker.in_flight.insert(cache_key.clone(), ());

                let w = Arc::clone(&worker);
                let sid = site_id.clone();
                let origin_clone = origin.clone();
                let host_clone = host.clone();
                let sem = semaphore.clone();
                let site_cfg = Arc::clone(&site_config);

                tokio::spawn(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => {
                            w.in_flight.remove(&cache_key);
                            return;
                        }
                    };

                    let start = Instant::now();

                    // Check if already cached
                    if w.cache_storage.get(&sid, &cache_key).await.is_some() {
                        PREFETCH_TOTAL
                            .with_label_values(&[sid.as_str(), "already_cached"])
                            .inc();
                        w.in_flight.remove(&cache_key);
                        return;
                    }

                    // Fetch from origin
                    let origin_url = build_origin_url(&origin_clone, &url, &host_clone);
                    match w.fetch_segment(&origin_url, &host_clone).await {
                        Ok((status, headers, body)) => {
                            let ttl = site_cfg.cache.default_ttl;
                            let meta = storage::build_cache_meta(
                                status,
                                &headers,
                                ttl,
                                body.len() as u64,
                            );
                            if let Err(e) =
                                w.cache_storage.put(&sid, &cache_key, &meta, body).await
                            {
                                log::warn!(
                                    "[Prefetch] cache put failed for {}: {}",
                                    cache_key, e
                                );
                                PREFETCH_TOTAL
                                    .with_label_values(&[sid.as_str(), "error"])
                                    .inc();
                            } else {
                                log::debug!(
                                    "[Prefetch] cached segment {} for site {}",
                                    cache_key, sid
                                );
                                PREFETCH_TOTAL
                                    .with_label_values(&[sid.as_str(), "success"])
                                    .inc();
                            }
                        }
                        Err(e) => {
                            log::warn!("[Prefetch] fetch failed for {}: {}", url, e);
                            PREFETCH_TOTAL
                                .with_label_values(&[sid.as_str(), "error"])
                                .inc();
                        }
                    }

                    PREFETCH_DURATION
                        .with_label_values(&[sid.as_str()])
                        .observe(start.elapsed().as_secs_f64());

                    w.in_flight.remove(&cache_key);
                });
            }
        });
    }

    /// Fetch a segment from origin via HTTP.
    async fn fetch_segment(
        &self,
        url: &str,
        host: &str,
    ) -> Result<(u16, Vec<(String, String)>, Vec<u8>), reqwest::Error> {
        let resp = self
            .http_client
            .get(url)
            .header("Host", host)
            .header("User-Agent", "Nozdormu-Prefetch/1.0")
            .send()
            .await?;

        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = resp.bytes().await?.to_vec();

        Ok((status, headers, body))
    }
}

/// Parse URL into (host, path) components.
fn parse_url_components(url: &str, default_host: &str) -> (String, String) {
    if let Some(rest) = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")) {
        if let Some(slash_pos) = rest.find('/') {
            let host = &rest[..slash_pos];
            let path = &rest[slash_pos..];
            return (host.to_string(), path.to_string());
        }
    }
    // Relative or path-only URL
    (default_host.to_string(), url.to_string())
}

/// Build the full origin URL for fetching a segment.
fn build_origin_url(origin: &OriginConfig, segment_url: &str, _host: &str) -> String {
    if segment_url.starts_with("http://") || segment_url.starts_with("https://") {
        // Absolute URL — rewrite to go through origin
        if let Some(rest) = segment_url
            .strip_prefix("http://")
            .or_else(|| segment_url.strip_prefix("https://"))
        {
            if let Some(slash_pos) = rest.find('/') {
                let path = &rest[slash_pos..];
                let scheme = match origin.protocol {
                    cdn_common::OriginProtocol::Https => "https",
                    cdn_common::OriginProtocol::Http => "http",
                };
                return format!("{}://{}:{}{}", scheme, origin.host, origin.port, path);
            }
        }
    }

    // Relative path
    let scheme = match origin.protocol {
        cdn_common::OriginProtocol::Https => "https",
        cdn_common::OriginProtocol::Http => "http",
    };
    format!("{}://{}:{}{}", scheme, origin.host, origin.port, segment_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url_components_absolute() {
        let (host, path) = parse_url_components(
            "http://cdn.example.com/video/seg0.ts",
            "default.com",
        );
        assert_eq!(host, "cdn.example.com");
        assert_eq!(path, "/video/seg0.ts");
    }

    #[test]
    fn test_parse_url_components_relative() {
        let (host, path) = parse_url_components("/video/seg0.ts", "default.com");
        assert_eq!(host, "default.com");
        assert_eq!(path, "/video/seg0.ts");
    }

    #[test]
    fn test_build_origin_url_absolute() {
        let origin = OriginConfig {
            id: "o1".into(),
            host: "backend.example.com".into(),
            port: 8080,
            weight: 10,
            protocol: cdn_common::OriginProtocol::Http,
            backup: false,
            enabled: true,
            sni: None,
            verify_ssl: false,
            target_labels: vec![],
        };
        let url = build_origin_url(
            &origin,
            "http://cdn.example.com/video/seg0.ts",
            "cdn.example.com",
        );
        assert_eq!(url, "http://backend.example.com:8080/video/seg0.ts");
    }

    #[test]
    fn test_build_origin_url_relative() {
        let origin = OriginConfig {
            id: "o1".into(),
            host: "backend.example.com".into(),
            port: 443,
            weight: 10,
            protocol: cdn_common::OriginProtocol::Https,
            backup: false,
            enabled: true,
            sni: None,
            verify_ssl: true,
            target_labels: vec![],
        };
        let url = build_origin_url(&origin, "/video/seg0.ts", "cdn.example.com");
        assert_eq!(url, "https://backend.example.com:443/video/seg0.ts");
    }
}
