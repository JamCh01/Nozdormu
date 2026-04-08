#!/usr/bin/env bash
# ============================================================
# Load global config sections into etcd
# ============================================================
set -euo pipefail

PREFIX="${CDN_ETCD_PREFIX:-/nozdormu}"
ETCD_CONTAINER="${ETCD_CONTAINER:-docker-etcd1-1}"

etcdctl() {
    docker exec "$ETCD_CONTAINER" etcdctl "$@"
}

echo "[Global] Loading global config into etcd (prefix: $PREFIX)..."

etcdctl put "${PREFIX}/global/redis" '{
  "mode": "standalone",
  "standalone": {
    "host": "127.0.0.1",
    "port": 6379
  },
  "password": null,
  "db": 0,
  "connect_timeout_ms": 5000,
  "send_timeout_ms": 5000,
  "read_timeout_ms": 5000,
  "pool_size": 50
}'

etcdctl put "${PREFIX}/global/security" '{
  "waf_default_mode": "block",
  "cc_default_rate": 100,
  "cc_default_window": 60,
  "cc_default_block_duration": 600,
  "cc_challenge_secret": "test_e2e_secret_key_12345",
  "trusted_proxies": ["127.0.0.0/8"]
}'

etcdctl put "${PREFIX}/global/balancer" '{
  "lb_algorithm": "round_robin",
  "retries": 2,
  "dns_nameservers": ["8.8.8.8", "8.8.4.4"],
  "health_check_interval": 10,
  "health_check_timeout": 5,
  "healthy_threshold": 2,
  "unhealthy_threshold": 3
}'

etcdctl put "${PREFIX}/global/proxy" '{
  "connect_timeout": 10,
  "send_timeout": 60,
  "read_timeout": 60,
  "websocket_timeout": 3600,
  "sse_timeout": 86400,
  "grpc_timeout": 300
}'

etcdctl put "${PREFIX}/global/logging" '{
  "push_to_redis": false,
  "stream_max_len": 10000
}'

etcdctl put "${PREFIX}/global/compression" '{
  "enabled": false,
  "algorithms": ["zstd", "brotli", "gzip"],
  "level": 6,
  "min_size": 256,
  "compressible_types": ["text/*", "application/json", "application/javascript"]
}'

echo "[Global] Done loading global config"
