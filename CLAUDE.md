# Nozdormu CDN — Development Guide

## Project Overview

Nozdormu is a high-performance CDN reverse proxy built on Cloudflare's Pingora framework, migrating from an OpenResty/Lua stack. It provides WAF, rate limiting (CC), caching, SSL/TLS management, multi-protocol support, and dynamic configuration via etcd.

## Build & Test

```bash
# Build (requires Rust 1.84+, OpenSSL dev headers)
cargo build

# Run all tests (432 unit/integration tests across 7 crates)
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
  cdn-config/       Node config, GlobalConfig (etcd), LiveConfig (ArcSwap), etcd watcher
  cdn-cache/        Cache strategy, key generation, OSS/S3 storage, bulk purge (ListObjectsV2 + Multi-Object Delete)
  cdn-image/        Image optimization: params parsing, Accept negotiation, decode/resize/encode (image + fast_image_resize)
  cdn-streaming/    Video streaming: URL signing auth (Type A/B/C), MP4→HLS dynamic packaging, HLS/DASH prefetch
  cdn-middleware/    WAF (IP/GeoIP + body inspection), CC rate limiting, redirects, header manipulation
  cdn-proxy/        Main binary — Pingora ProxyHttp, balancer, DNS, SSL, active health probes, admin API
```

Dependency flow: `cdn-common` ← `cdn-config` ← `cdn-cache` / `cdn-image` / `cdn-streaming` / `cdn-middleware` ← `cdn-proxy`

## Configuration Examples

Detailed JSON examples with inline documentation for every config option:

- **Global configs** (`docs/global/`): redis, redis_standalone, security, balancer, proxy, cache, ssl, logging, compression, image_optimization
- **Site configs** (`docs/site/`): basic, origins, lb_round_robin, lb_ip_hash, lb_random, lb_backup_failover, waf, waf_log_mode, waf_body, cc, cache, protocol, redirect, headers, compression, image_optimization, range, streaming_auth, streaming_packaging, streaming_prefetch, ssl, full

## Architecture Essentials

- **Config hot-reload**: `ArcSwap<LiveConfig>` swapped atomically from etcd watch events. WAF IP sets are compiled per-config and cached thread-locally.
- **Hybrid config loading**: Cluster-shared config (Redis, security, balancer, timeouts, cache, SSL, logging) loaded from etcd `{prefix}/global/*` at startup, with env vars as override. Priority: **env > etcd > default**. Bootstrap params (node identity, etcd address, paths, log level, secrets) are CLI-only.
- **Per-request context**: `ProxyCtx` carries all state through Pingora's `request_filter` → `upstream_peer` → `response_filter` → `logging` callbacks. Includes `start_time: Instant` for request duration tracking.
- **Hybrid CC counting**: Local moka cache (zero-latency) + async Redis sync every 10 increments. Redis counters use Lua INCRBY+EXPIRE for atomic TTL.
- **Passive health checks**: Tracked in `logging()` callback via DashMap. 5xx or connection failure = failure, else success. Thresholds from global `BalancerConfig`.
- **Active health checks**: `ActiveHealthCheckService` (Pingora `BackgroundService`) runs a supervisor loop that reconciles per-origin probe tasks against `LiveConfig` every 5s. Each probe task runs HTTP GET or TCP connect at configurable intervals with initial jitter. Uses per-site thresholds and calls `HealthChecker::set_status()` on threshold crossing. Active and passive coexist — both write to the same `HealthChecker` DashMap; last write wins.
- **Cache key generation**: Direct hashing — components are fed directly into the MD5 hasher via `hasher.update()` calls, avoiding intermediate String allocations. Only the final hex-encoded result allocates.
- **Admin API**: Served on the proxy port under `/_admin/` prefix (no separate listener). Bearer token auth required — token configured via etcd `{prefix}/global/security` `admin_token`. If no token is configured, all admin requests return 403. Handlers are pure async functions dispatched in `request_filter` before site routing. Routes: `POST /reload`, `POST /ssl/clear-cache`, `GET /site/{id}`, `GET /upstream/health`, `PUT /upstream/health/{site_id}/{origin_id}`, `GET /cc/blocked`, `POST /cache/purge`, `GET /cache/purge/status/{task_id}`, `GET /cache/purge/status`. Request body reading capped at 1 MB.
- **Cache purge**: Exact URL purge (synchronous, regenerates cache key from request components) and site-wide purge (async background task). Site purge uses Redis SCAN (cursor-based, non-blocking) to discover cache keys, then S3 Multi-Object Delete for bulk body removal. Falls back to S3 ListObjectsV2 when Redis is unavailable. `PurgeTaskTracker` (DashMap) tracks background task progress with auto-eviction. Redis meta key cleanup uses concurrent batched deletion (chunks of 100).
- **Response compression**: gzip/Brotli/Zstd via `response_body_filter` streaming. Negotiated per-request from `Accept-Encoding`. Two-tier config: per-site override + global default. Skipped for WebSocket/SSE/gRPC and non-compressible MIME types. Encoder `write_chunk()` returns `Result` — errors are logged and compression is disabled mid-stream rather than silently producing corrupt output.
- **Image optimization**: On-the-fly resize/crop, format conversion (JPEG/PNG/WebP/AVIF), quality adjustment via query params (`?w=200&h=150&fit=cover&fmt=webp&q=80&dpr=2`). Auto-negotiates output format from `Accept` header (AVIF > WebP > original). Full-body buffering in `response_body_filter` (images require complete data before processing). Image path and compression path are mutually exclusive. Uses `image` crate for decode/encode + `fast_image_resize` for SIMD-optimized resize. Graceful degradation: serves original image on processing failure. Cache key correctness is automatic (query params already included in MD5 hash). DPR-aware dimension clamping ensures `width * dpr <= max_width`. WebP encoding is lossless-only (quality param ignored, logged at debug level).
- **Range requests**: Client resume/continuation support (RFC 7233). Parses `Range: bytes=X-Y` headers (single range, suffix, open-ended; multi-range rejected). Pass-through to origin on cache miss; serves byte ranges from cached full bodies on cache hit. Advertises `Accept-Ranges: bytes` on cacheable responses. Supports `If-Range` conditional (strong ETag comparison + HTTP-date). Range is mutually exclusive with both image optimization (image wins) and compression (byte offsets refer to uncompressed content). OSS Range GET (`get_object_range`) avoids loading entire cached files into memory. Per-site `RangeConfig { enabled, chunk_size }` — `chunk_size` reserved for Phase 2 chunked origin-pull.
- **Video streaming optimization**: Three features in `cdn-streaming` crate:
  - **Edge Auth (URL Signing)**: Type A (`/{timestamp}/{hash}/{path}`), Type B (`?auth_key={ts}-{rand}-{uid}-{hash}`), Type C (`/{hash}/{timestamp}/{path}`). HMAC-SHA256 with constant-time comparison. Validated in `request_filter` before cache lookup — unauthorized requests never touch cache. Auth tokens stripped from upstream path. Per-site `StreamingAuthConfig { auth_type, auth_key, expire_time }`.
  - **Dynamic Packaging (MP4→HLS)**: In-house MP4 atom parser (moov/trak/stbl), generates fMP4 init segments + media segments + m3u8 playlists. Triggered by `?format=hls` or `Accept: application/vnd.apple.mpegurl`. Full MP4 buffered in `response_body_filter` (same pattern as image optimization). Each HLS variant (manifest/init/segment N) gets a distinct cache key. Mutually exclusive with image optimization (image wins) and Range (packaging wins). Per-site `DynamicPackagingConfig { segment_duration, max_mp4_size }`. Invalid sample offsets are handled gracefully (size zeroed in trun to maintain ISO BMFF consistency).
  - **Smart Prefetching**: Parses HLS m3u8 and DASH mpd manifests from response bodies, extracts segment URLs, fires background `tokio::spawn` tasks to fetch next N segments from origin via reqwest and store in cache. Shadow-copies manifest body (doesn't consume — client receives at wire speed). Per-site concurrency via `Semaphore`, deduplication via `DashMap` `entry()` API (atomic check-and-insert). Response body capped at 256 MB to prevent OOM from malicious origins. Per-site `PrefetchConfig { prefetch_count, concurrency_limit }`.
- **Request body inspection**: Two-phase body checking in `waf/body.rs`. Phase 1: Content-Length pre-check in `request_filter` (early 413 before body transfer). Phase 2: `request_body_filter` buffers first 8KB for magic-bytes detection via `infer` crate (~200 file types, zero deps), enforces size limit incrementally per chunk. Supports allowed/blocked MIME type lists with wildcard matching (`image/*`), content-type mismatch detection (declared vs detected at type-family level). Per-site `BodyInspectionConfig { max_body_size, allowed_content_types, blocked_content_types, inspect_methods }`.

## Key Patterns

- **Error handling**: `CdnError` (thiserror) in cdn-common, `anyhow` for ad-hoc errors, `pingora::Error` at proxy boundaries. `RedisOps::get()` returns `Result<Option<String>, String>` to distinguish missing keys from connection failures. Other Redis operations degrade gracefully (return Ok on failure).
- **Sensitive data**: `SecurityConfig`, `RedisConfig`, `EtcdConfig`, `EabCredentials`, `StreamingAuthConfig` use custom `Debug` impls that redact secrets. Never log these with `{:?}` raw.
- **Header operations**: `apply_header_op` macro generates both request and response variants. Must use Pingora's `insert_header()` method (not direct `headers.insert()`) to keep `header_name_map` in sync — direct mutation causes a panic in `write_response_header`.
- **Request IDs**: Lightweight `timestamp-counter` format (no UUID syscall overhead).
- **Log queue**: Bounded `mpsc::channel(8192)` with a single background consumer that batches entries (up to 64) for Redis XADD. Backpressure drops entries rather than spawning unbounded tasks.
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

The proxy listens on `0.0.0.0:6188` (HTTP) and metrics on `0.0.0.0:6190` (Prometheus).
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
- Streaming packaging tests use hand-crafted minimal MP4 byte arrays for parser tests, verify fMP4 box structure (ftyp/moov/moof/mdat), segment sample ranges, and HLS playlist generation
- Streaming prefetch tests use manifest parsing (HLS m3u8 line-based, DASH mpd XML), URL resolution (relative/absolute), and origin URL construction
- Body inspection tests use magic bytes (JPEG/PNG/PDF/GIF/ZIP signatures), Content-Length size checks, wildcard MIME matching, content-type mismatch detection, and edge cases (empty body, unknown type, disabled config)

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

Covers: WAF (IP/GeoIP/block/log), CC (block/challenge/per-path), cache rules, LB (round-robin/ip-hash/backup), compression (gzip/br/zstd negotiation + skip conditions), redirects (domain/exact/prefix/regex), headers (set/add/remove/append + variable substitution), protocol (force_https + exclude paths), admin API, and cross-feature interactions.
