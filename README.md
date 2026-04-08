# Nozdormu CDN

High-performance CDN reverse proxy built on [Pingora](https://github.com/cloudflare/pingora), designed as a production replacement for OpenResty/Lua-based CDN systems.

## Features

- **Dynamic Configuration** -- Site configs and cluster-shared settings stored in etcd, hot-reloaded via ArcSwap with zero downtime; bootstrap params via CLI flags, env vars override etcd for per-node control
- **WAF** -- IP/CIDR whitelist/blacklist (prefix trie O(log n)), GeoIP country/region/ASN filtering, fail-closed country whitelist
- **Rate Limiting (CC)** -- Hybrid local+Redis counters, JS challenge (HMAC-SHA256), per-path rules with longest-prefix matching
- **Caching** -- Dual-backend: Redis metadata + S3/OSS body storage, rule-based TTL (path/extension/regex), Cache-Control compliance
- **Multi-Protocol** -- HTTP, WebSocket, SSE, gRPC (native + gRPC-Web) with per-protocol timeout and header handling
- **Load Balancing** -- Weighted round-robin, IP hash, random; passive health checks with automatic failover to backup origins
- **SSL/TLS** -- Multi-provider ACME (Let's Encrypt, ZeroSSL, Buypass, Google), automatic renewal, distributed locking
- **Redirects** -- Three-tier engine: domain redirect, protocol enforcement (HTTP/HTTPS), URL rules (exact/prefix/regex/domain)
- **Header Manipulation** -- Request/response header rules with variable substitution (`${client_ip}`, `${host}`, `${cache_status}`, etc.)
- **Observability** -- Prometheus metrics, Redis Streams request logging, per-request ID tracking
- **Compression** -- gzip, Brotli, Zstandard with `Accept-Encoding` negotiation; per-site config with global default; auto-skip for WebSocket/SSE/gRPC and non-compressible types
- **Admin API** -- Config reload, health status, CC state inspection; Bearer token auth with constant-time comparison

## Architecture

```
crates/
  cdn-common        Shared types, error handling, RedisOps trait
  cdn-config        Node config, GlobalConfig (etcd), LiveConfig (ArcSwap), etcd watcher
  cdn-cache         Cache strategy, key generation, S3/OSS client (AWS Sig v4)
  cdn-middleware     WAF engine, CC engine, redirect engine, header rules
  cdn-proxy          Main binary: Pingora proxy, balancer, DNS, SSL, admin API
```

### Request Flow

```
Client -> Pingora Listener
  -> Health/ACME/Admin endpoints (short-circuit)
  -> Site routing (domain -> SiteConfig via exact/wildcard match)
  -> Client IP extraction (XFF anti-spoofing, configurable trusted proxies)
  -> WAF check (IP trie -> GeoIP -> ASN -> country -> region)
  -> CC check (ban cache -> challenge verify -> counter -> threshold)
  -> Redirect check (domain -> protocol -> URL rules)
  -> Protocol detection (gRPC > WebSocket > SSE > HTTP)
  -> Cache lookup (key generation -> Redis meta -> OSS body)
  -> Load balancer (health filter -> algorithm -> DNS resolve -> HttpPeer)
  -> Upstream request (header injection, protocol-specific headers)
  -> Response filter (header rules, cache write, security headers)
  -> Logging (Prometheus metrics, Redis Streams, passive health update)
```

## Requirements

- Rust 1.84+ (stable)
- OpenSSL development headers (`libssl-dev` / `openssl-devel`)
- etcd v3.5+ (configuration store)
- Redis 7+ with Sentinel (optional, for distributed CC counters and log streaming)
- MaxMind GeoLite2 databases (optional, for WAF geo-filtering)

## Quick Start

### Docker (recommended)

```bash
# Start everything (CDN + etcd cluster + Redis Sentinel)
docker compose --profile dev up

# Production build
docker compose --profile prod up -d
```

### From Source

```bash
# Install dependencies (Debian/Ubuntu)
apt-get install -y libssl-dev pkg-config cmake protobuf-compiler

# Start infrastructure
docker compose --profile infra up -d

# Build and run
cargo build --release
./target/release/cdn-proxy -c config/default.yaml \
  --env development --log-level info
```

### Verify

```bash
# Health check
curl http://localhost:6188/health
# -> OK

# Prometheus metrics
curl http://localhost:6190/metrics

# Admin API (localhost only)
curl http://localhost:8080/upstream/health
```

## Configuration

Nozdormu uses a three-tier configuration system with priority: **CLI arg > etcd > default**.

### Tier 1: Bootstrap (CLI Arguments)

These are required before etcd is available and are passed via command-line flags. Run `cdn-proxy --help` for the full list.

| Category | CLI Flags |
|----------|-----------|
| Node Identity | `--node-id`, `--node-labels`, `--env` |
| etcd | `--etcd-endpoints`, `--etcd-prefix`, `--etcd-username`, `--etcd-password` |
| Paths | `--cert-path`, `--geoip-path`, `--log-path` |
| Log Level | `--log-level` |

### Tier 2: Cluster-Shared (etcd Global Config)

Loaded from etcd at startup under `{prefix}/global/*`. Shared across all nodes. Env vars override etcd values for per-node emergency overrides.

| etcd Key | Contents |
|----------|----------|
| `{prefix}/global/redis` | Redis mode, sentinels, host, port, timeouts, pool size |
| `{prefix}/global/security` | WAF mode, CC defaults, trusted proxies, CC challenge secret, admin token |
| `{prefix}/global/balancer` | LB algorithm, retries, DNS, health check thresholds |
| `{prefix}/global/proxy` | Connect/send/read/WebSocket/SSE/gRPC timeouts |
| `{prefix}/global/cache` | OSS endpoint, bucket, region, SSL, TTL, max size |
| `{prefix}/global/ssl` | ACME environment, email, providers, renewal days |
| `{prefix}/global/logging` | Redis log push, stream max length |
| `{prefix}/global/compression` | Compression algorithms, level, min size, MIME types |

Example: set Redis config for the entire cluster:

```bash
etcdctl put /nozdormu/global/redis '{
  "mode": "sentinel",
  "sentinel": {
    "master_name": "mymaster",
    "nodes": ["sentinel1:26379", "sentinel2:26379", "sentinel3:26379"]
  },
  "password": null,
  "db": 0,
  "pool_size": 200
}'
```

If no global keys exist in etcd, the system falls back to defaults (fully backward compatible).

### Tier 3: Site Configuration (etcd Per-Site)

Sites are stored as JSON in etcd at `{prefix}/sites/{site_id}`. Example:

```json
{
  "site_id": "example",
  "enabled": true,
  "port": 80,
  "domains": ["example.com", "*.example.com"],
  "origins": [
    {
      "id": "origin-1",
      "host": "backend.example.com",
      "port": 443,
      "protocol": "https",
      "weight": 10
    }
  ],
  "load_balancer": {
    "algorithm": "round_robin",
    "retries": 2
  },
  "cache": {
    "enabled": true,
    "default_ttl": 3600,
    "rules": [
      { "type": "extension", "match": ["js", "css", "png"], "ttl": 86400 },
      { "type": "path", "match": "/api", "ttl": 0 }
    ]
  },
  "waf": {
    "enabled": true,
    "mode": "block",
    "rules": {
      "ip_blacklist": ["192.168.0.0/16"],
      "country_whitelist": ["US", "JP", "DE"]
    }
  },
  "cc": {
    "enabled": true,
    "default_rate": 100,
    "default_window": 60,
    "rules": [
      { "path": "/api/login", "rate": 5, "window": 60, "action": "challenge" }
    ]
  },
  "protocol": {
    "force_https": { "enable": true, "https_port": 443, "redirect_code": 301, "exclude_paths": ["/health", "/.well-known/"] },
    "websocket": { "enable": true },
    "grpc": { "enabled": true }
  },
  "compression": {
    "enabled": true,
    "algorithms": ["zstd", "brotli", "gzip"],
    "level": 6,
    "min_size": 256,
    "compressible_types": ["text/*", "application/json", "application/javascript", "image/svg+xml"]
  }
}
```

Changes are picked up automatically via etcd watch (no restart needed). Manual reload is available via the admin API:

```bash
curl -X POST http://localhost:8080/reload
```

## Ports

| Port | Service |
|------|---------|
| 6188 | HTTP proxy |
| 6190 | Prometheus metrics |
| 8080 | Admin API (localhost only) |

## Development

```bash
# Run unit/integration tests (244 tests)
cargo test

# Lint
cargo clippy --workspace

# Format
cargo fmt --all

# Dev mode with hot-reload (requires cargo-watch)
cargo watch -x "run -p cdn-proxy -- -c config/default.yaml"
```

### E2E Functional Tests

End-to-end tests exercise the full proxy with real infrastructure (etcd, Redis, Python backends, GeoIP):

```bash
# Start infra + backends + proxy
bash tests/e2e/setup.sh

# Run all 79 tests (WAF, CC, cache, LB, compression, redirects, headers, admin)
bash tests/e2e/run_tests.sh

# Run specific test groups
bash tests/e2e/run_tests.sh waf cc compress

# Stop everything
bash tests/e2e/teardown.sh
```

See [CLAUDE.md](CLAUDE.md) for detailed development guidelines.

## License

Internal project. Not published.
