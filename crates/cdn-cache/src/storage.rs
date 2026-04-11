use crate::key::cache_object_path;
use crate::oss::{OssClient, OssError};
use cdn_common::RedisOps;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Cached response metadata (stored in Redis).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMeta {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub cached_at: i64,  // Unix timestamp
    pub expires_at: i64, // Unix timestamp
    pub size: u64,
    pub etag: Option<String>,
    #[serde(default)]
    pub stale_while_revalidate: u64, // seconds; 0 = disabled
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Full cached response (metadata + body).
#[derive(Debug)]
pub struct CachedResponse {
    pub meta: CacheMeta,
    pub body: Vec<u8>,
}

/// Cache storage: Redis metadata + OSS/S3 data body.
///
/// Read flow:
/// 1. Redis GET meta → miss = None
/// 2. Check expires_at → expired = None
/// 3. OSS GET body → failure = cleanup Redis, None
///
/// Write flow:
/// 1. OSS PUT body
/// 2. Redis SETEX meta (TTL = expires_at - now)
pub struct CacheStorage {
    oss: Option<Arc<OssClient>>,
    redis: Option<Arc<dyn RedisOps>>,
}

impl CacheStorage {
    /// Create a new CacheStorage with an OSS client and optional Redis.
    pub fn new(oss: Option<Arc<OssClient>>, redis: Option<Arc<dyn RedisOps>>) -> Self {
        Self { oss, redis }
    }

    /// Create a disabled CacheStorage (no backend).
    pub fn disabled() -> Self {
        Self {
            oss: None,
            redis: None,
        }
    }

    /// Check if storage is available.
    pub fn is_available(&self) -> bool {
        self.oss.is_some()
    }

    /// Read a cached response.
    /// Returns None on miss, expiry, or error.
    pub async fn get(&self, site_id: &str, cache_key: &str) -> Option<CachedResponse> {
        let oss = self.oss.as_ref()?;
        let object_path = cache_object_path(site_id, cache_key);
        let redis_key = format!("nozdormu:cache:meta:{}:{}", site_id, cache_key);

        // Step 1: Try Redis metadata first
        if let Some(ref redis) = self.redis {
            if let Ok(Some(meta_json)) = redis.get(&redis_key).await {
                if let Ok(meta) = serde_json::from_str::<CacheMeta>(&meta_json) {
                    let now = chrono::Utc::now().timestamp();
                    if meta.expires_at <= now {
                        log::debug!("[Cache] expired meta: {}", object_path);
                        let redis = Arc::clone(redis);
                        let rk = redis_key.clone();
                        tokio::spawn(async move {
                            if let Err(e) = redis.del(&rk).await {
                                log::warn!("[Cache] background Redis DEL failed: {} - {}", rk, e);
                            }
                        });
                        return None;
                    }
                    // Step 2: Fetch body from OSS
                    match oss.get_object(&object_path).await {
                        Ok(body) => return Some(CachedResponse { meta, body }),
                        Err(e) => {
                            log::warn!("[Cache] OSS get failed after meta hit: {}", e);
                            let redis = Arc::clone(redis);
                            let rk = redis_key.clone();
                            tokio::spawn(async move {
                                if let Err(e) = redis.del(&rk).await {
                                    log::warn!(
                                        "[Cache] background Redis DEL failed: {} - {}",
                                        rk,
                                        e
                                    );
                                }
                            });
                            return None;
                        }
                    }
                }
            }
        }

        // Fallback: OSS-only path (no Redis metadata available).
        // Use a short TTL since we can't verify actual expiry.
        match oss.get_object(&object_path).await {
            Ok(data) => {
                let now = chrono::Utc::now().timestamp();
                log::debug!("[Cache] OSS hit (no meta, stale-eligible): {}", object_path);
                Some(CachedResponse {
                    meta: CacheMeta {
                        status: 200,
                        headers: HashMap::new(),
                        cached_at: now,
                        expires_at: now + 60, // 60s conservative TTL — caller should revalidate
                        size: data.len() as u64,
                        etag: None,
                        stale_while_revalidate: 0,
                        tags: Vec::new(),
                    },
                    body: data,
                })
            }
            Err(OssError::NotFound) => {
                log::debug!("[Cache] miss: {}", object_path);
                None
            }
            Err(e) => {
                log::error!("[Cache] OSS get error: {} - {}", object_path, e);
                None
            }
        }
    }

    /// Read a cached response, allowing stale reads within the SWR window.
    /// Returns `(CachedResponse, is_stale)` where `is_stale=true` means the entry
    /// is expired but within the `stale-while-revalidate` window.
    pub async fn get_with_stale(
        &self,
        site_id: &str,
        cache_key: &str,
    ) -> Option<(CachedResponse, bool)> {
        let oss = self.oss.as_ref()?;
        let object_path = cache_object_path(site_id, cache_key);
        let redis_key = format!("nozdormu:cache:meta:{}:{}", site_id, cache_key);

        // Step 1: Try Redis metadata first
        if let Some(ref redis) = self.redis {
            if let Ok(Some(meta_json)) = redis.get(&redis_key).await {
                if let Ok(meta) = serde_json::from_str::<CacheMeta>(&meta_json) {
                    let now = chrono::Utc::now().timestamp();
                    if meta.expires_at <= now {
                        // Check stale-while-revalidate window
                        let stale_deadline = meta.expires_at + meta.stale_while_revalidate as i64;
                        if meta.stale_while_revalidate > 0 && stale_deadline > now {
                            // Within SWR window — return stale response
                            match oss.get_object(&object_path).await {
                                Ok(body) => return Some((CachedResponse { meta, body }, true)),
                                Err(e) => {
                                    log::warn!("[Cache] OSS get failed for stale entry: {}", e);
                                    return None;
                                }
                            }
                        }
                        // Beyond SWR window — truly expired
                        log::debug!("[Cache] expired meta: {}", object_path);
                        let redis = Arc::clone(redis);
                        let rk = redis_key.clone();
                        tokio::spawn(async move {
                            if let Err(e) = redis.del(&rk).await {
                                log::warn!("[Cache] background Redis DEL failed: {} - {}", rk, e);
                            }
                        });
                        return None;
                    }
                    // Not expired — fresh hit
                    match oss.get_object(&object_path).await {
                        Ok(body) => return Some((CachedResponse { meta, body }, false)),
                        Err(e) => {
                            log::warn!("[Cache] OSS get failed after meta hit: {}", e);
                            let redis = Arc::clone(redis);
                            let rk = redis_key.clone();
                            tokio::spawn(async move {
                                if let Err(e) = redis.del(&rk).await {
                                    log::warn!(
                                        "[Cache] background Redis DEL failed: {} - {}",
                                        rk,
                                        e
                                    );
                                }
                            });
                            return None;
                        }
                    }
                }
            }
        }

        // Fallback: OSS-only path (no Redis metadata available).
        match oss.get_object(&object_path).await {
            Ok(data) => {
                let now = chrono::Utc::now().timestamp();
                log::debug!("[Cache] OSS hit (no meta, stale-eligible): {}", object_path);
                Some((
                    CachedResponse {
                        meta: CacheMeta {
                            status: 200,
                            headers: HashMap::new(),
                            cached_at: now,
                            expires_at: now + 60,
                            size: data.len() as u64,
                            etag: None,
                            stale_while_revalidate: 0,
                            tags: Vec::new(),
                        },
                        body: data,
                    },
                    false,
                ))
            }
            Err(OssError::NotFound) => None,
            Err(e) => {
                log::error!("[Cache] OSS get error: {} - {}", object_path, e);
                None
            }
        }
    }

    /// Read a byte range from a cached response body.
    /// Uses OSS Range GET to avoid loading the entire file into memory.
    /// Returns None on miss, expiry, or error.
    pub async fn get_range(
        &self,
        site_id: &str,
        cache_key: &str,
        start: u64,
        end: u64,
    ) -> Option<(CacheMeta, Vec<u8>)> {
        let oss = self.oss.as_ref()?;
        let object_path = cache_object_path(site_id, cache_key);
        let redis_key = format!("nozdormu:cache:meta:{}:{}", site_id, cache_key);

        // Must have valid, non-expired meta
        if let Some(ref redis) = self.redis {
            if let Ok(Some(meta_json)) = redis.get(&redis_key).await {
                if let Ok(meta) = serde_json::from_str::<CacheMeta>(&meta_json) {
                    let now = chrono::Utc::now().timestamp();
                    if meta.expires_at <= now {
                        return None;
                    }
                    match oss.get_object_range(&object_path, start, end).await {
                        Ok(body) => return Some((meta, body)),
                        Err(e) => {
                            log::warn!("[Cache] OSS range get failed: {}", e);
                            return None;
                        }
                    }
                }
            }
        }
        None
    }

    /// Write a response to cache.
    /// This is called asynchronously (fire-and-forget via tokio::spawn).
    pub async fn put(
        &self,
        site_id: &str,
        cache_key: &str,
        meta: &CacheMeta,
        body: Vec<u8>,
    ) -> Result<(), String> {
        let oss = self.oss.as_ref().ok_or("OSS not configured")?;
        let object_path = cache_object_path(site_id, cache_key);

        // Determine content type from meta headers
        let content_type = meta
            .headers
            .get("content-type")
            .map(|s| s.as_str())
            .unwrap_or("application/octet-stream");

        // Step 1: OSS PUT body
        oss.put_object(&object_path, body, content_type)
            .await
            .map_err(|e| format!("OSS put error: {}", e))?;

        // Step 2: Redis SETEX meta
        let meta_json =
            serde_json::to_string(meta).map_err(|e| format!("meta serialize error: {}", e))?;
        let redis_key = format!("nozdormu:cache:meta:{}:{}", site_id, cache_key);
        let ttl = (meta.expires_at - chrono::Utc::now().timestamp()).clamp(1, 86400 * 365) as u64;

        if let Some(ref redis) = self.redis {
            if let Err(e) = redis.setex(&redis_key, ttl, &meta_json).await {
                log::warn!("[Cache] Redis SETEX failed: {} - {}", redis_key, e);
            }

            // Write tag index entries
            if !meta.tags.is_empty() {
                for tag in &meta.tags {
                    let tag_key = format!("nozdormu:cache:tag:{}:{}", site_id, tag);
                    if let Err(e) = redis.sadd(&tag_key, cache_key).await {
                        log::warn!("[Cache] Redis SADD failed: {} - {}", tag_key, e);
                    }
                    // Set TTL on tag set to match meta TTL + 1h buffer
                    if let Err(e) = redis.expire(&tag_key, ttl + 3600).await {
                        log::warn!("[Cache] Redis EXPIRE failed on tag key: {}", e);
                    }
                }
            }
        }

        log::debug!(
            "[Cache] stored: {} ({} bytes, ttl={}s)",
            object_path,
            meta.size,
            ttl
        );

        Ok(())
    }

    /// Delete a cached response.
    pub async fn delete(&self, site_id: &str, cache_key: &str) -> Result<(), String> {
        let oss = self.oss.as_ref().ok_or("OSS not configured")?;
        let object_path = cache_object_path(site_id, cache_key);

        // Clean up tag index entries before deleting meta
        let redis_key = format!("nozdormu:cache:meta:{}:{}", site_id, cache_key);
        if let Some(ref redis) = self.redis {
            if let Ok(Some(meta_json)) = redis.get(&redis_key).await {
                if let Ok(meta) = serde_json::from_str::<CacheMeta>(&meta_json) {
                    for tag in &meta.tags {
                        let tag_key = format!("nozdormu:cache:tag:{}:{}", site_id, tag);
                        if let Err(e) = redis.srem(&tag_key, cache_key).await {
                            log::warn!("[Cache] Redis SREM failed: {} - {}", tag_key, e);
                        }
                    }
                }
            }
        }

        oss.delete_object(&object_path)
            .await
            .map_err(|e| format!("OSS delete error: {}", e))?;

        // Redis DEL meta
        if let Some(ref redis) = self.redis {
            if let Err(e) = redis.del(&redis_key).await {
                log::warn!("[Cache] Redis DEL failed: {} - {}", redis_key, e);
            }
        }

        Ok(())
    }

    /// Delete multiple cache entries by their cache keys.
    /// Used for site-wide purge after discovering keys via Redis SCAN.
    /// Returns the count of successfully deleted entries.
    pub async fn delete_many(&self, site_id: &str, cache_keys: &[String]) -> Result<u32, String> {
        if cache_keys.is_empty() {
            return Ok(0);
        }

        let mut deleted = 0u32;

        // Delete from OSS in batch
        if let Some(ref oss) = self.oss {
            let object_paths: Vec<String> = cache_keys
                .iter()
                .map(|k| cache_object_path(site_id, k))
                .collect();
            match oss.delete_objects_batch(&object_paths).await {
                Ok(count) => deleted = count,
                Err(e) => {
                    log::error!(
                        "[Cache] batch OSS delete failed for site {}: {}",
                        site_id,
                        e
                    );
                    return Err(format!("OSS batch delete error: {}", e));
                }
            }
        }

        // Delete Redis meta keys concurrently in batches
        if let Some(ref redis) = self.redis {
            for chunk in cache_keys.chunks(100) {
                let mut handles = Vec::with_capacity(chunk.len());
                for key in chunk {
                    let redis_key = format!("nozdormu:cache:meta:{}:{}", site_id, key);
                    let r = Arc::clone(redis);
                    handles.push(tokio::spawn(async move {
                        if let Err(e) = r.del(&redis_key).await {
                            log::warn!("[Cache] Redis DEL failed: {} - {}", redis_key, e);
                        }
                    }));
                }
                for h in handles {
                    let _ = h.await;
                }
            }
        }

        Ok(deleted)
    }

    /// Delete all cache entries for a site by listing OSS objects.
    /// Fallback path when Redis is unavailable for SCAN.
    /// Returns the count of deleted objects.
    pub async fn delete_site_oss_only(&self, site_id: &str) -> Result<u32, String> {
        let oss = self.oss.as_ref().ok_or("OSS not configured")?;
        let prefix = format!("cache/{}/", site_id);

        let keys = oss
            .list_objects(&prefix, 1_000_000)
            .await
            .map_err(|e| format!("OSS list error: {}", e))?;

        if keys.is_empty() {
            return Ok(0);
        }

        log::info!(
            "[Cache] purge site {} via OSS listing: {} objects found",
            site_id,
            keys.len()
        );

        let deleted = oss
            .delete_objects_batch(&keys)
            .await
            .map_err(|e| format!("OSS batch delete error: {}", e))?;

        Ok(deleted)
    }

    /// Purge all cache entries matching a tag.
    /// 1. SMEMBERS to get all cache keys for the tag
    /// 2. delete_many() to remove them
    /// 3. DEL the tag set itself
    pub async fn delete_by_tag(&self, site_id: &str, tag: &str) -> Result<u64, String> {
        let redis = self.redis.as_ref().ok_or("Redis not available")?;
        let tag_key = format!("nozdormu:cache:tag:{}:{}", site_id, tag);

        let cache_keys: Vec<String> = redis
            .smembers(&tag_key)
            .await
            .map_err(|e| format!("SMEMBERS failed: {}", e))?;

        if cache_keys.is_empty() {
            return Ok(0);
        }

        let deleted = self.delete_many(site_id, &cache_keys).await?;

        // Remove the tag set itself
        if let Err(e) = redis.del(&tag_key).await {
            log::warn!("[Cache] Redis DEL tag key failed: {} - {}", tag_key, e);
        }

        Ok(deleted as u64)
    }
}

/// Build CacheMeta from response status and headers.
pub fn build_cache_meta(
    status: u16,
    headers: &[(String, String)],
    ttl: u64,
    body_size: u64,
    stale_while_revalidate: u64,
    tags: Vec<String>,
) -> CacheMeta {
    let now = chrono::Utc::now().timestamp();
    let etag = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("etag"))
        .map(|(_, v)| v.clone());

    let filtered_headers: HashMap<String, String> = headers
        .iter()
        .filter(|(k, _)| {
            !crate::strategy::EXCLUDED_RESPONSE_HEADERS
                .iter()
                .any(|ex| k.eq_ignore_ascii_case(ex))
        })
        .cloned()
        .collect();

    CacheMeta {
        status,
        headers: filtered_headers,
        cached_at: now,
        expires_at: {
            // Cap TTL to 10 years to prevent i64 overflow
            const MAX_TTL: u64 = 86400 * 365 * 10;
            let capped = ttl.min(MAX_TTL) as i64;
            now.saturating_add(capped)
        },
        size: body_size,
        etag,
        stale_while_revalidate,
        tags,
    }
}

/// Parse cache tags from response headers.
/// Checks `Surrogate-Key` and `Cache-Tag` headers (space-separated, deduplicated).
pub fn parse_cache_tags(headers: &[(String, String)]) -> Vec<String> {
    let mut tags = Vec::new();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("surrogate-key") || name.eq_ignore_ascii_case("cache-tag") {
            for tag in value.split_whitespace() {
                let t = tag.trim();
                if !t.is_empty() && !tags.contains(&t.to_string()) {
                    tags.push(t.to_string());
                }
            }
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cache_tags_surrogate_key() {
        let headers = vec![("surrogate-key".to_string(), "tag1 tag2 tag3".to_string())];
        let tags = parse_cache_tags(&headers);
        assert_eq!(tags, vec!["tag1", "tag2", "tag3"]);
    }

    #[test]
    fn test_parse_cache_tags_cache_tag() {
        let headers = vec![("Cache-Tag".to_string(), "product homepage".to_string())];
        let tags = parse_cache_tags(&headers);
        assert_eq!(tags, vec!["product", "homepage"]);
    }

    #[test]
    fn test_parse_cache_tags_both_headers() {
        let headers = vec![
            ("Surrogate-Key".to_string(), "tag1 tag2".to_string()),
            ("Cache-Tag".to_string(), "tag3 tag1".to_string()), // tag1 is duplicate
        ];
        let tags = parse_cache_tags(&headers);
        assert_eq!(tags, vec!["tag1", "tag2", "tag3"]);
    }

    #[test]
    fn test_parse_cache_tags_empty() {
        let headers: Vec<(String, String)> = vec![];
        let tags = parse_cache_tags(&headers);
        assert!(tags.is_empty());
    }

    #[test]
    fn test_parse_cache_tags_no_matching_headers() {
        let headers = vec![("content-type".to_string(), "text/html".to_string())];
        let tags = parse_cache_tags(&headers);
        assert!(tags.is_empty());
    }

    #[test]
    fn test_cache_meta_serde_backward_compat() {
        // Existing entries without stale_while_revalidate and tags should deserialize
        let json = r#"{
            "status": 200,
            "headers": {},
            "cached_at": 1000,
            "expires_at": 2000,
            "size": 100,
            "etag": null
        }"#;
        let meta: CacheMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.stale_while_revalidate, 0);
        assert!(meta.tags.is_empty());
    }

    #[test]
    fn test_cache_meta_serde_with_new_fields() {
        let json = r#"{
            "status": 200,
            "headers": {},
            "cached_at": 1000,
            "expires_at": 2000,
            "size": 100,
            "etag": null,
            "stale_while_revalidate": 60,
            "tags": ["product", "homepage"]
        }"#;
        let meta: CacheMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.stale_while_revalidate, 60);
        assert_eq!(meta.tags, vec!["product", "homepage"]);
    }

    #[test]
    fn test_build_cache_meta_with_tags() {
        let headers = vec![
            ("content-type".to_string(), "text/html".to_string()),
            ("surrogate-key".to_string(), "tag1 tag2".to_string()),
        ];
        let meta = build_cache_meta(
            200,
            &headers,
            3600,
            1024,
            60,
            vec!["tag1".to_string(), "tag2".to_string()],
        );
        assert_eq!(meta.status, 200);
        assert_eq!(meta.stale_while_revalidate, 60);
        assert_eq!(meta.tags, vec!["tag1", "tag2"]);
        assert!(meta.expires_at > meta.cached_at);
    }
}
