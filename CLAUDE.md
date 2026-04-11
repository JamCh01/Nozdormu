# Nozdormu CDN — Development Guide

## Project Overview

Nozdormu is a high-performance CDN reverse proxy built on Cloudflare's Pingora framework, migrating from an OpenResty/Lua stack. It provides WAF, rate limiting (CC), caching, SSL/TLS management, multi-protocol support, and dynamic configuration via etcd.

## Build & Test

```bash
# Build (requires Rust 1.84+, OpenSSL dev headers)
cargo build

# Run all tests (568 unit/integration tests across 9 crates)
cargo test

# Run tests for a specific crate
cargo test -p cdn-middleware
cargo test -p cdn-proxy

# Run a specific test
cargo test -p cdn-middleware cc::tests::test_over_rate_blocks

# Check without building
cargo check

# Lint
cargo clippy --workspace

# Format
cargo fmt --all

# E2E tests (requires Docker for etcd + Redis)
bash tests/e2e/setup.sh       # Start infra, backends, proxy
bash tests/e2e/run_tests.sh   # Run 79 curl-based functional tests
bash tests/e2e/teardown.sh    # Stop everything
```

## Workspace Structure

```
crates/
  cdn-common/       Shared types (SiteConfig, CdnError, RedisOps trait)
  cdn-config/       Node config, GlobalConfig (etcd), LiveConfig (ArcSwap), etcd watcher, config version history
  cdn-cache/        Cache strategy, key generation, OSS/S3 storage, bulk purge (ListObjectsV2 + Multi-Object Delete)
  cdn-log/          Multi-backend multi-channel logging: LogSink trait, Redis/Kafka/RabbitMQ/NATS/Pulsar sinks, 8 independent channels, 4-phase timing
  cdn-image/        Image optimization: params parsing, Accept negotiation, decode/resize/encode (image + fast_image_resize)
  cdn-streaming/    Video streaming: URL signing auth (Type A/B/C), MP4→HLS dynamic packaging, HLS/DASH prefetch, live fMP4 generation API
  cdn-ingest/       Live ingest: RTMP/SRT push streaming → HLS/LL-HLS output, in-memory ring buffer, stream key auth
  cdn-middleware/    WAF (IP/GeoIP + body inspection), CC rate limiting, redirects, header manipulation
  cdn-proxy/        Main binary — Pingora ProxyHttp, balancer, DNS, SSL, active health probes, admin API
```

Dependency flow: `cdn-common` ← `cdn-config` ← `cdn-cache` / `cdn-log` / `cdn-image` / `cdn-streaming` / `cdn-middleware` ← `cdn-ingest` ← `cdn-proxy`

## Configuration Examples

Detailed JSON examples with inline documentation for every config option:

- **Global configs** (`docs/global/`): redis, redis_standalone, security, balancer, proxy, cache, ssl, logging, compression, image_optimization, ingest
- **Site configs** (`docs/site/`): basic, origins, lb_round_robin, lb_ip_hash, lb_random, lb_least_conn, lb_backup_failover, waf, waf_log_mode, waf_body, cc, cache, protocol, redirect, headers, compression, image_optimization, range, streaming_auth, streaming_packaging, streaming_prefetch, ssl, error_pages, webhook, full

## Architecture Essentials

- **Config hot-reload**: `ArcSwap<LiveConfig>` swapped atomically from etcd watch events. WAF IP sets are compiled per-config and cached thread-locally.
- **Hybrid config loading**: Cluster-shared config (Redis, security, balancer, timeouts, cache, SSL, logging) loaded from etcd `{prefix}/global/*` at startup, with env vars as override. Priority: **env > etcd > default**. Bootstrap params (node identity, etcd address, paths, log level, secrets) are CLI-only.
- **Per-request context**: `ProxyCtx` carries all state through Pingora's `request_filter` → `upstream_peer` → `response_filter` → `logging` callbacks. Includes `start_time: Instant` for request duration tracking.
- **Hybrid CC counting**: Local moka cache (zero-latency) + async Redis sync every 10 increments. Redis counters use Lua INCRBY+EXPIRE for atomic TTL.
- **Passive health checks**: Tracked in `logging()` callback via DashMap. 5xx or connection failure = failure, else success. Thresholds from global `BalancerConfig`.
- **Active health checks**: `ActiveHealthCheckService` (Pingora `BackgroundService`) runs a supervisor loop that reconciles per-origin probe tasks against `LiveConfig` every 5s. Each probe task runs HTTP GET or TCP connect at configurable intervals with initial jitter. Uses per-site thresholds and calls `HealthChecker::set_status()` on threshold crossing. Active and passive coexist — both write to the same `HealthChecker` DashMap; last write wins.
- **Least-connections balancing**: `DynamicBalancer.active_conns: DashMap<String, AtomicU32>` tracks per-origin active connection counts (key: `"site_id\0origin_id"`). Incremented in `upstream_peer()` after `select_peer()`, decremented in `logging()` (always called, covers all exit paths including errors). `select_least_conn()` picks the origin with the fewest active connections; ties broken by weight (higher wins), then position (first wins). Saturating decrement prevents underflow from edge cases.
- **Consistent hash (IP Hash)**: `select_ip_hash()` uses a Ketama-style consistent hash ring. Each origin gets `weight * 40` virtual nodes hashed via SipHash (`std::hash::DefaultHasher`). Ring is a sorted `Vec<(u64, usize)>` built per-request from healthy candidates; lookup via `binary_search_by_key` + wrap-around. Adding/removing an origin remaps only ~1/N of requests. Weight=0 origins get no vnodes; if all weights are 0, falls back to simple hash modulo.
- **Adaptive weight adjustment**: `DynamicBalancer.origin_stats: DashMap<String, OriginStats>` tracks per-origin sliding window of `(latency_ms, is_error)` samples (default 100). `effective_weight()` computes a multiplier in [0.1, 1.0] from P99 latency and error rate via `penalty_factor()` (linear interpolation between threshold and 4x threshold). All 4 LB algorithms (`select_round_robin`, `select_ip_hash`, `select_weighted_random`, `select_least_conn`) receive pre-computed `eff_weights: &[u32]` instead of reading `origin.weight` directly. Data collected in `logging()` callback via `record_response()`. Stale data (>60s no traffic) or <10 samples → no penalty. Floor of 1 prevents starvation. Per-site `AdaptiveWeightConfig { enabled, window_size, latency_threshold_ms, error_threshold }` in `LoadBalancerConfig`, disabled by default. Admin endpoint: `GET /_admin/adaptive/weights`. Prometheus gauge: `cdn_adaptive_weight_effective`.
- **Cache key generation**: Direct hashing — components are fed directly into the MD5 hasher via `hasher.update()` calls, avoiding intermediate String allocations. Only the final hex-encoded result allocates.
- **Cache read/write**: Wired in `request_filter` (read) and `response_body_filter` (write). Read path uses `CacheStorage::get_with_stale()` which returns `(CachedResponse, is_stale)`. On hit, `serve_cached_response()` short-circuits upstream. On miss, body is shadow-copied during streaming and written to cache via `tokio::spawn` at `end_of_stream`. Response cacheability checked in `response_filter` via `check_response_cacheability()` + `adjust_ttl()`.
- **Stale-While-Revalidate**: Parses `Cache-Control: stale-while-revalidate=N` directive. When a cache entry is expired but within the SWR window (`expires_at + swr > now`), serves the stale response immediately and triggers `trigger_background_revalidation()` — a `tokio::spawn` task that fetches from origin via `reqwest` and updates the cache. `CacheStatus::Stale` variant tracks this. `CacheMeta.stale_while_revalidate` field persisted in Redis (backward-compatible via `#[serde(default)]`).
- **Request coalescing**: `DashMap<String, Arc<CoalescingEntry>>` on `CdnProxy` keyed by cache_key. First request for a cache miss becomes the "leader" (`ctx.is_coalescing_leader = true`), subsequent requests wait on `tokio::sync::Notify` with 30s timeout. Triple-guarded cleanup: `response_body_filter` end_of_stream, `fail_to_connect` final failure, and `logging` callback (safety net). Prometheus counter `cdn_cache_coalescing_total` with labels: leader/waited_hit/waited_miss/timeout.
- **Cache tags**: `CacheMeta.tags: Vec<String>` parsed from `Surrogate-Key` / `Cache-Tag` response headers (space-separated). Redis SET reverse index: `nozdormu:cache:tag:{site_id}:{tag} -> SET { cache_keys }`. Written via `SADD` in `CacheStorage::put()`, cleaned via `SREM` in `delete()`. Tag-based purge via `POST /_admin/cache/purge` with `{"type": "tag", "site_id": "...", "tag": "..."}` — calls `CacheStorage::delete_by_tag()` (SMEMBERS → delete_many → DEL tag set). `RedisOps` trait extended with `sadd`, `smembers`, `srem`, `expire`.
- **Cache warm API**: `POST /_admin/cache/warm` accepts `{"site_id": "...", "urls": [{"host": "...", "path": "...", "query_string": "..."}]}` (max 1000 URLs). Spawns background task with `Semaphore(10)` concurrency. Fetches from first healthy origin via `reqwest`, writes to cache. `WarmTaskTracker` (DashMap, same pattern as `PurgeTaskTracker`) tracks progress. Status endpoints: `GET /_admin/cache/warm/status/{task_id}`, `GET /_admin/cache/warm/status`.
- **Config version management**: Versioned snapshots of site configs stored in etcd at `{prefix}/config_history/{site_id}/v/{version:010}`. Per-site monotonic version counter at `{prefix}/config_history/{site_id}/meta`, atomically incremented via etcd Txn CAS (compare mod_revision, retry up to 5 times). Snapshots include version number, RFC 3339 timestamp, etcd revision, change type (created/updated/deleted/rollback), and full SiteConfig JSON as `serde_json::Value` (schema-agnostic for forward compatibility). Max 50 versions per site; older versions pruned via etcd range delete. Version capture is fire-and-forget (`tokio::spawn`) in the etcd watch loop — never blocks config updates. Rollback writes the historical config back to `{prefix}/sites/{site_id}`, triggering normal watch → LiveConfig update. `pending_change_types: Mutex<HashMap<(String, i64), ConfigChangeType>>` on `EtcdConfigManager` coordinates rollback change type tagging between the admin API and the watcher. Admin endpoints: `GET /config/history/{site_id}`, `GET /config/history/{site_id}/{version}`, `POST /config/rollback/{site_id}/{version}`.
- **Admin API**: Served on the proxy port under `/_admin/` prefix (no separate listener). Bearer token auth required — token configured via etcd `{prefix}/global/security` `admin_token`. If no token is configured, all admin requests return 403. Handlers are pure async functions dispatched in `request_filter` before site routing. Routes: `POST /reload`, `POST /ssl/clear-cache`, `GET /site/{id}`, `GET /upstream/health`, `PUT /upstream/health/{site_id}/{origin_id}`, `GET /cc/blocked`, `POST /cache/purge` (url/site/tag), `GET /cache/purge/status/{task_id}`, `GET /cache/purge/status`, `POST /cache/warm`, `GET /cache/warm/status/{task_id}`, `GET /cache/warm/status`, `GET /config/history/{site_id}`, `GET /config/history/{site_id}/{version}`, `POST /config/rollback/{site_id}/{version}`, `GET /adaptive/weights`, `GET /log/status`. Request body reading capped at 1 MB. OpenAPI 3.1 spec at `GET /_admin/openapi.json` (no auth, CORS enabled), Swagger UI at `GET /_admin/swagger` (no auth). Static spec embedded via `include_str!` from `docs/openapi.json`.
- **Cache purge**: Exact URL purge (synchronous, regenerates cache key from request components), site-wide purge (async background task), and tag-based purge (synchronous, SMEMBERS → batch delete). Site purge uses Redis SCAN (cursor-based, non-blocking) to discover cache keys, then S3 Multi-Object Delete for bulk body removal. Falls back to S3 ListObjectsV2 when Redis is unavailable. `PurgeTaskTracker` (DashMap) tracks background task progress with auto-eviction. Redis meta key cleanup uses concurrent batched deletion (chunks of 100).
- **Response compression**: gzip/Brotli/Zstd via `response_body_filter` streaming. Negotiated per-request from `Accept-Encoding`. Two-tier config: per-site override + global default. Skipped for WebSocket/SSE/gRPC and non-compressible MIME types. Encoder `write_chunk()` returns `Result` — errors are logged and compression is disabled mid-stream rather than silently producing corrupt output.
- **Image optimization**: On-the-fly resize/crop, format conversion (JPEG/PNG/WebP/AVIF), quality adjustment via query params (`?w=200&h=150&fit=cover&fmt=webp&q=80&dpr=2`). Auto-negotiates output format from `Accept` header (AVIF > WebP > original). Full-body buffering in `response_body_filter` (images require complete data before processing). Image path and compression path are mutually exclusive. Uses `image` crate for decode/encode + `fast_image_resize` for SIMD-optimized resize. Graceful degradation: serves original image on processing failure. Cache key correctness is automatic (query params already included in MD5 hash). DPR-aware dimension clamping ensures `width * dpr <= max_width`. WebP encoding is lossless-only (quality param ignored, logged at debug level).
- **Range requests**: Client resume/continuation support (RFC 7233). Parses `Range: bytes=X-Y` headers (single range, suffix, open-ended; multi-range rejected). Pass-through to origin on cache miss; serves byte ranges from cached full bodies on cache hit. Advertises `Accept-Ranges: bytes` on cacheable responses. Supports `If-Range` conditional (strong ETag comparison + HTTP-date). Range is mutually exclusive with both image optimization (image wins) and compression (byte offsets refer to uncompressed content). OSS Range GET (`get_object_range`) avoids loading entire cached files into memory. Per-site `RangeConfig { enabled, chunk_size }` — `chunk_size` reserved for Phase 2 chunked origin-pull.
- **Video streaming optimization**: Three features in `cdn-streaming` crate:
  - **Edge Auth (URL Signing)**: Type A (`/{timestamp}/{hash}/{path}`), Type B (`?auth_key={ts}-{rand}-{uid}-{hash}`), Type C (`/{hash}/{timestamp}/{path}`). HMAC-SHA256 with constant-time comparison. Validated in `request_filter` before cache lookup — unauthorized requests never touch cache. Auth tokens stripped from upstream path. Per-site `StreamingAuthConfig { auth_type, auth_key, expire_time }`.
  - **Dynamic Packaging (MP4→HLS)**: In-house MP4 atom parser (moov/trak/stbl), generates fMP4 init segments + media segments + m3u8 playlists. Triggered by `?format=hls` or `Accept: application/vnd.apple.mpegurl`. Full MP4 buffered in `response_body_filter` (same pattern as image optimization). Each HLS variant (manifest/init/segment N) gets a distinct cache key. Mutually exclusive with image optimization (image wins) and Range (packaging wins). Per-site `DynamicPackagingConfig { segment_duration, max_mp4_size, ll_hls }`. Invalid sample offsets are handled gracefully (size zeroed in trun to maintain ISO BMFF consistency). **LL-HLS (Low-Latency HLS)**: When `ll_hls.enabled`, playlist uses `#EXT-X-VERSION:9` with `#EXT-X-PART` tags (partial segments), `#EXT-X-PART-INF` (PART-TARGET), `#EXT-X-SERVER-CONTROL` (CAN-BLOCK-RELOAD, PART-HOLD-BACK=3×PART-TARGET). Each full segment is subdivided into parts by `part_duration` (default 0.5s). Parts are standalone fMP4 fragments (moof+mdat) sharing the same init segment. `INDEPENDENT=YES` on parts starting with keyframes. Part URLs: `?format=hls&segment=N&part=P`. `_HLS_msn`/`_HLS_part` query params recognized and stripped (VOD: immediate response). `LlHlsConfig { enabled, part_duration }` per-site, disabled by default.
  - **Smart Prefetching**: Parses HLS m3u8 and DASH mpd manifests from response bodies, extracts segment URLs, fires background `tokio::spawn` tasks to fetch next N segments from origin via reqwest and store in cache. Shadow-copies manifest body (doesn't consume — client receives at wire speed). Per-site concurrency via `Semaphore`, deduplication via `DashMap` `entry()` API (atomic check-and-insert). Response body capped at 256 MB to prevent OOM from malicious origins. Per-site `PrefetchConfig { prefetch_count, concurrency_limit }`.
- **Live ingest (RTMP/SRT)**: `cdn-ingest` crate provides RTMP (TCP:1935) and SRT (UDP) push streaming. Two Pingora `BackgroundService` instances (`RtmpIngestService`, `SrtIngestService`) run inside the cdn-proxy process. RTMP uses `rml_rtmp` crate for protocol handling (handshake, chunk stream, AMF commands); SRT uses `srt-tokio` (pure Rust). Both demux incoming streams to extract H.264 video (via FLV tags for RTMP, MPEG-TS for SRT) and AAC audio frames. `LiveSegmenter` accumulates frames and produces fMP4 segments (reusing `cdn-streaming`'s box-writing helpers via `generate_live_media_segment()`). Segments stored in `LiveStreamStore` — a `DashMap<String, Arc<RwLock<LiveStream>>>` with per-stream ring buffer (`VecDeque<LiveSegment>`, configurable max_segments). LL-HLS partial segments (`#EXT-X-PART`) supported with blocking playlist reload (`_HLS_msn`/`_HLS_part` → `oneshot` waiters). HTTP serving via `/live/{app}/{stream}.m3u8` path interception in `request_filter` (before site routing). Stream key authentication maps `rtmp://host/{app}/{stream_key}` to configured `StreamKeyEntry` list. Node-level config in `CdnConfig.ingest` (YAML). Prometheus metrics: `cdn_ingest_connections_total`, `cdn_ingest_frames_total`, `cdn_ingest_segments_total`, `cdn_ingest_active_streams`.
- **Custom error pages**: Per-site `error_pages: HashMap<u16, String>` in `SiteConfig` maps HTTP status codes (400-599) to inline HTML content. All error response methods (`serve_not_found`, `serve_forbidden`, `serve_too_early`, `serve_payload_too_large`, `serve_too_many_requests`, `serve_bad_request`) check the site's `error_pages` config. If a custom page exists for the status code, it's served as `text/html; charset=utf-8`; otherwise the default plain-text body is used. The unified `serve_error_with_page()` helper handles the lookup and response construction. Pre-routing errors (ACME, admin API, site-not-found) pass `None` for error_pages since no site config is available yet. `request_body_filter` errors (413/403) also use custom pages via `serve_error_with_page()`. CC challenge (503) is excluded — it has its own JS-based HTML. Validation in `schema.rs` ensures status codes are in 400-599 range.
- **Request body inspection**: Two-phase body checking in `waf/body.rs`. Phase 1: Content-Length pre-check in `request_filter` (early 413 before body transfer). Phase 2: `request_body_filter` buffers first 8KB for magic-bytes detection via `infer` crate (~200 file types, zero deps), enforces size limit incrementally per chunk. Supports allowed/blocked MIME type lists with wildcard matching (`image/*`), content-type mismatch detection (declared vs detected at type-family level). Per-site `BodyInspectionConfig { max_body_size, allowed_content_types, blocked_content_types, inspect_methods }`.
- **TLS listener & 0-RTT Early Data**: Optional downstream TLS listener via `tls_listen` config field. Uses Pingora's `TlsSettings::with_callbacks()` + `TlsAccept` trait for dynamic certificate provisioning — `CdnTlsAccept` in `ssl/tls_accept.rs` looks up certs via `CertManager` (SNI → exact → wildcard → default). TLS 1.3 0-RTT enabled via `set_max_early_data()` on `SslAcceptorBuilder` when `early_data: true`. After handshake, `SSL_get_early_data_status()` (FFI) stores result in `SslDigest.extension` as `TlsHandshakeData`. In `request_filter`, non-idempotent methods (POST/PUT/DELETE/PATCH) on 0-RTT connections are rejected with 425 Too Early. Idempotent 0-RTT requests get `Early-Data: 1` upstream header per RFC 8470. Prometheus counter `cdn_early_data_requests_total` tracks accepted/rejected. Both TCP and TLS listeners share the same `CdnProxy` instance.
- **ACME certificate issuance**: Complete ACME v2 (RFC 8555) protocol flow in `ssl/acme.rs` using `instant-acme` crate. Multi-provider rotation (Let's Encrypt → ZeroSSL → Buypass → Google) with EAB support. Account credentials persisted in Redis (`nozdormu:acme:account:{provider}:{email_hash}`, TTL 365d) for reuse across nodes. HTTP-01 challenge tokens stored in `ChallengeStore` (in-memory DashMap), served at `/.well-known/acme-challenge/` before WAF/routing. CSR generated via `rcgen` (ECDSA P-256). Polling with exponential backoff (2s→10s, 300s timeout). Challenge tokens cleaned up after validation or on error.
- **Certificate auto-renewal**: `RenewalBgService` (Pingora `BackgroundService`) runs `RenewalManager::check_and_renew()` — first check 60s after startup, then every 24h. Two-level Redis distributed locking: scan lock (`renewal:scan`, TTL 1h) ensures only one node scans, per-domain lock (`renewal:{domain}`, TTL 10min) prevents duplicate issuance. Double-check after lock acquisition. After successful renewal, `CertManager::invalidate(domain)` flushes the moka cache so TLS callbacks pick up the new cert. 5s rate-limit delay between renewals. Prometheus counters: `cdn_acme_issuance_total`, `cdn_acme_issuance_duration_seconds`, `cdn_acme_renewal_total`.
- **Webhook notifications**: Per-site `WebhookConfig` in `SiteConfig` with `enabled`, `urls`, `secret`, `timeout_secs`, `max_retries`. `dispatch()` in `admin/webhook.rs` is fire-and-forget: checks config, spawns `tokio::spawn` per URL for delivery with exponential backoff retry. Three event sources: `RenewalManager` looks up site by domain via `LiveConfig::match_site()` and emits `CertRenewalSuccess`/`CertRenewalFailure`; `update_probe_state()` in `health_probe.rs` returns `Option<bool>` on health transitions, `probe_loop` looks up site config and emits `HealthStatusChange`; `purge_site_background()` receives site's `WebhookConfig` and emits `CachePurgeCompleted`. Optional HMAC-SHA256 signature (`ring::hmac`) in `X-Webhook-Signature` header. `WebhookDeliveryTracker` (DashMap, 1h auto-eviction) tracks delivery status. Admin endpoints: `GET /_admin/webhook/events`, `POST /_admin/webhook/test/{site_id}`. Prometheus counter: `cdn_webhook_delivery_total` with labels `[event_type, result]`.

## Key Patterns

- **Error handling**: `CdnError` (thiserror) in cdn-common, `anyhow` for ad-hoc errors, `pingora::Error` at proxy boundaries. `RedisOps::get()` returns `Result<Option<String>, String>` to distinguish missing keys from connection failures. Other Redis operations degrade gracefully (return Ok on failure).
- **Sensitive data**: `SecurityConfig`, `RedisConfig`, `EtcdConfig`, `EabCredentials`, `StreamingAuthConfig` use custom `Debug` impls that redact secrets. Never log these with `{:?}` raw.
- **Header operations**: `apply_header_op` macro generates both request and response variants. Must use Pingora's `insert_header()` method (not direct `headers.insert()`) to keep `header_name_map` in sync — direct mutation causes a panic in `write_response_header`.
- **Request IDs**: Lightweight `timestamp-counter` format (no UUID syscall overhead).
- **Log queue**: Bounded `mpsc::channel(8192)` with a single background consumer that batches `(destination, json)` tuples by destination before calling `sink.send(dest, &batch)`. Backpressure drops entries rather than spawning unbounded tasks.
- **Multi-backend multi-channel logging**: `cdn-log` crate provides `LogSink` trait with `send(destination, entries)` method and 5 implementations: `RedisStreamSink` (destination = stream_key), `KafkaSink` (destination = topic), `RabbitMQSink` (destination = routing_key), `NatsSink` (destination = subject), `PulsarSink` (destination = topic, lazy per-topic producer via DashMap). Each backend behind a cargo feature flag. **8 independent log channels**: `client_to_cdn`, `cdn_to_origin`, `origin_to_cdn`, `cdn_to_client` (4-phase timing), `waf`, `cc`, `cache` (event logs), `access` (full LogEntry). Each channel has `{ enabled: bool, destination: String }` in `LogChannelsConfig`. `push_log(channels, entry)` serializes only enabled channels' sub-structs and routes to their destinations. Phase channels only emit when timing data exists (e.g., cache hits skip all 4 phase channels). `LogBackendConfig.channels()` accessor provides unified access across all backend variants.
- **4-phase request timing**: `ProxyCtx` has 3 timing markers: `upstream_start` (set in `upstream_peer()`), `upstream_connected` (set in `upstream_request_filter()`), `upstream_response_received` (set in `response_filter()`). `compute_phase_timings()` derives 4 durations: `client_to_cdn_ms` (request processing), `cdn_to_origin_ms` (DNS + connect), `origin_to_cdn_ms` (origin TTFB), `cdn_to_client_ms` (response delivery). All `Option<f64>` — `None` when request short-circuits (cache hit, WAF block). Logged in `LogEntry` alongside total `duration_ms`.
- **Thread-local caches**: Regex patterns (true LRU with promotion on access, cap 256), WAF IP sets (keyed by version counter to avoid ABA, retain-based eviction instead of full clear).

## Running Locally

```bash
# Start infrastructure (etcd + Redis Sentinel)
docker compose --profile infra up -d

# Run the proxy
cargo run -p cdn-proxy -- -c config/default.yaml \
  --env development --log-level info

# Or with Docker (dev mode with hot-reload)
docker compose --profile dev up
```

The proxy listens on `0.0.0.0:6188` (HTTP) and metrics on `0.0.0.0:6190` (Prometheus). Optional TLS listener on `0.0.0.0:6189` (HTTPS) when `tls_listen` is configured in `config/default.yaml` with certificates in `--cert-path`.
Admin API is served on the proxy port under `/_admin/` prefix (Bearer token auth required, configured via etcd `{prefix}/global/security` `admin_token`).

## Configuration

Configuration uses a three-tier system:

1. **Bootstrap CLI args** (always from CLI): `--node-id`, `--etcd-endpoints`, `--cert-path`, `--log-level`, etc.
2. **Cluster-shared config** (from etcd `{prefix}/global/*`): Redis, security (CC secret, admin token, trusted proxies), balancer, timeouts, cache/OSS, SSL/ACME, logging
3. **Site config** (from etcd `{prefix}/sites/{site_id}`): per-site WAF, CC, cache, origins, domains, redirects

Startup flow: `CdnOpt::parse()` → `BootstrapConfig::from_cli()` → `load_global_config(etcd)` → `NodeConfig::from_etcd_and_cli()`.

Env vars override etcd values for cluster-shared configs (for emergency single-node overrides). If no etcd global keys exist, defaults are used.

Critical production requirements:
- `cc_challenge_secret` **must** be set in etcd `{prefix}/global/security` (startup warns with default in non-development)
- `--etcd-endpoints` is required
- `--node-id` is required

## Code Style

- Rust 2021 edition, `rustfmt` with `max_width=100`
- Clippy: `too-many-arguments-threshold=8`, `type-complexity-threshold=300`
- Prefer `&str` / `&[T]` over owned types in function parameters on hot paths
- Use `Arc<dyn RedisOps>` for cross-crate Redis access (avoids circular deps)
- Thread-local caches use `RefCell<(HashMap, VecDeque)>` for LRU eviction
- Prometheus labels: use status class (`2xx`/`3xx`/etc.), never raw status codes

## Testing

- Unit tests live alongside source in `#[cfg(test)] mod tests`
- Integration tests in `crates/cdn-proxy/tests/integration_test.rs`
- Tests that need async: `#[tokio::test]`
- WAF/CC tests use `WafEngine::without_geoip()` and in-memory state (no Redis needed)
- Cache tests use `CacheConfig::default()` with rule overrides
- Health probe tests use `HealthChecker` directly with `ProbeState` threshold logic (no network needed)
- Purge tests use `PurgeTaskTracker` directly and serde deserialization (no Redis/OSS needed)
- Image tests use in-memory generated JPEG/PNG test images, verify decode/resize/encode round-trips and dimension correctness
- Range tests use pure unit tests for parsing, resolution, Content-Range formatting, If-Range validation, and body slicing (no network needed)
- Streaming auth tests use sign/validate roundtrips with HMAC-SHA256, expiry checks, tampered hash detection, cross-type validation (Type A URL fails Type C)
- Streaming packaging tests use hand-crafted minimal MP4 byte arrays for parser tests, verify fMP4 box structure (ftyp/moov/moof/mdat), segment sample ranges, HLS playlist generation, LL-HLS partial segment generation (part sample range tiling, part smaller than full segment), and LL-HLS playlist tags (EXT-X-PART, EXT-X-PART-INF, EXT-X-SERVER-CONTROL, INDEPENDENT=YES)
- Streaming prefetch tests use manifest parsing (HLS m3u8 line-based, DASH mpd XML), URL resolution (relative/absolute), and origin URL construction
- Body inspection tests use magic bytes (JPEG/PNG/PDF/GIF/ZIP signatures), Content-Length size checks, wildcard MIME matching, content-type mismatch detection, and edge cases (empty body, unknown type, disabled config)
- Config history tests use serde roundtrips (ConfigVersionSnapshot, VersionMeta, ConfigChangeType), version key formatting (zero-padded), snapshot-to-summary conversion, and change type equality
- Ingest tests: config serde roundtrips, stream store (create/get/remove/ring buffer eviction/max streams/waiter notification), stream key auth (valid/invalid/disabled), H.264 SPS parsing (AVCC + Annex-B, stsd synthesis), AAC AudioSpecificConfig parsing (44100/48000 Hz, stsd synthesis), live HLS manifest generation (standard/LL-HLS/ENDLIST/PRELOAD-HINT), blocking playlist reload (already available/wait+notify/timeout), HTTP handler URL parsing (segment/part filenames, HLS params), segmenter frame push
- Error pages tests: serde roundtrip (HashMap<u16, String> deserialization), default empty map, validation (invalid status codes 200/600 rejected, valid 403/404/500/502 accepted)
- Webhook tests: WebhookEvent serde roundtrip (all 5 variants with event_type tag), event_type_label correctness, WebhookDeliveryTracker lifecycle (insert/delivered/failed), tracker auto-eviction (1h cutoff), tracker list sorted by created_at, notify with None sender (no-op), notify with closed sender (no panic), HMAC signature computation (deterministic, different secrets produce different output), WebhookConfig serde (full and minimal/defaults)

### E2E Tests

End-to-end functional tests in `tests/e2e/` exercise the full proxy with real infrastructure:

```
tests/e2e/
  setup.sh              Start docker-compose infra, Python backends, build & start proxy
  run_tests.sh          79 curl-based tests across 21 groups (can filter by group name)
  teardown.sh           Stop everything (--all to include docker-compose)
  lib.sh                Test framework (assertions, colors, counters)
  backends/server.py    Python HTTP server with 19 test endpoints
  configs/              15 site JSON configs + etcd loader scripts
```

Covers: WAF (IP/GeoIP/block/log), CC (block/challenge/per-path), cache rules, LB (round-robin/ip-hash/least-conn/backup), compression (gzip/br/zstd negotiation + skip conditions), redirects (domain/exact/prefix/regex), headers (set/add/remove/append + variable substitution), protocol (force_https + exclude paths), admin API, and cross-feature interactions.
