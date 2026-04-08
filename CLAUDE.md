# Nozdormu CDN — Development Guide

## Project Overview

Nozdormu is a high-performance CDN reverse proxy built on Cloudflare's Pingora framework, migrating from an OpenResty/Lua stack. It provides WAF, rate limiting (CC), caching, SSL/TLS management, multi-protocol support, and dynamic configuration via etcd.

## Build & Test

```bash
# Build (requires Rust 1.84+, OpenSSL dev headers)
cargo build

# Run all tests (244 unit/integration tests across 4 crates)
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
  cdn-cache/        Cache strategy, key generation, OSS/S3 storage
  cdn-middleware/    WAF, CC rate limiting, redirects, header manipulation
  cdn-proxy/        Main binary — Pingora ProxyHttp, balancer, DNS, SSL, admin API
```

Dependency flow: `cdn-common` ← `cdn-config` ← `cdn-cache` / `cdn-middleware` ← `cdn-proxy`

## Architecture Essentials

- **Config hot-reload**: `ArcSwap<LiveConfig>` swapped atomically from etcd watch events. WAF IP sets are compiled per-config and cached thread-locally.
- **Hybrid config loading**: Cluster-shared config (Redis, security, balancer, timeouts, cache, SSL, logging) loaded from etcd `{prefix}/global/*` at startup, with env vars as override. Priority: **env > etcd > default**. Bootstrap params (node identity, etcd address, paths, log level, secrets) are CLI-only.
- **Per-request context**: `ProxyCtx` carries all state through Pingora's `request_filter` → `upstream_peer` → `response_filter` → `logging` callbacks.
- **Hybrid CC counting**: Local moka cache (zero-latency) + async Redis sync every 10 increments. Redis counters use Lua INCRBY+EXPIRE for atomic TTL.
- **Passive health checks**: Tracked in `logging()` callback via DashMap. No separate probe goroutines yet.
- **Thread-local caches**: Regex patterns (LRU, cap 256), WAF IP sets (keyed by version counter to avoid ABA).
- **Response compression**: gzip/Brotli/Zstd via `response_body_filter` streaming. Negotiated per-request from `Accept-Encoding`. Two-tier config: per-site override + global default. Skipped for WebSocket/SSE/gRPC and non-compressible MIME types.

## Key Patterns

- **Error handling**: `CdnError` (thiserror) in cdn-common, `anyhow` for ad-hoc errors, `pingora::Error` at proxy boundaries. Redis operations degrade gracefully (return None/Ok on failure).
- **Sensitive data**: `SecurityConfig`, `RedisConfig`, `EtcdConfig`, `EabCredentials` use custom `Debug` impls that redact secrets. Never log these with `{:?}` raw.
- **Header operations**: `apply_header_op` macro generates both request and response variants. Must use Pingora's `insert_header()` method (not direct `headers.insert()`) to keep `header_name_map` in sync — direct mutation causes a panic in `write_response_header`.
- **Request IDs**: Lightweight `timestamp-counter` format (no UUID syscall overhead).

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
Admin API runs on `127.0.0.1:8080` (localhost only).

## Configuration

Configuration uses a three-tier system:

1. **Bootstrap CLI args** (always from CLI): `--node-id`, `--etcd-endpoints`, `--cert-path`, `--cc-challenge-secret`, etc.
2. **Cluster-shared config** (from etcd `{prefix}/global/*`): Redis, security, balancer, timeouts, cache/OSS, SSL/ACME, logging
3. **Site config** (from etcd `{prefix}/sites/{site_id}`): per-site WAF, CC, cache, origins, domains, redirects

Startup flow: `CdnOpt::parse()` → `BootstrapConfig::from_cli()` → `load_global_config(etcd)` → `NodeConfig::from_etcd_and_cli()`.

Env vars override etcd values for cluster-shared configs (for emergency single-node overrides). If no etcd global keys exist, defaults are used.

Critical production requirements:
- `--cc-challenge-secret` **must** be set (startup fails with default in non-development)
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

### E2E Tests

End-to-end functional tests in `tests/e2e/` exercise the full proxy with real infrastructure:

```
tests/e2e/
  setup.sh              Start docker-compose infra, Python backends, build & start proxy
  run_tests.sh          79 curl-based tests across 21 groups (can filter by group name)
  teardown.sh           Stop everything (--all to include docker-compose)
  lib.sh                Test framework (assertions, colors, counters)
  backends/server.py    Python HTTP server with 14 test endpoints
  configs/              15 site JSON configs + etcd loader scripts
```

Covers: WAF (IP/GeoIP/block/log), CC (block/challenge/per-path), cache rules, LB (round-robin/ip-hash/backup), compression (gzip/br/zstd negotiation + skip conditions), redirects (domain/exact/prefix/regex), headers (set/add/remove/append + variable substitution), protocol (force_https + exclude paths), admin API, and cross-feature interactions.
