#!/usr/bin/env bash
# ============================================================
# Nozdormu CDN — E2E Test Setup
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PIDS_FILE="$SCRIPT_DIR/.pids"
COMPOSE_DIR="$PROJECT_DIR/docker"

# etcd container name (from docker-compose)
ETCD_CONTAINER="docker-etcd1-1"
REDIS_CONTAINER="docker-redis-master-1"

cd "$PROJECT_DIR"

echo "============================================================"
echo " Nozdormu CDN — E2E Test Setup"
echo "============================================================"

# Clean up any previous run
if [[ -f "$PIDS_FILE" ]]; then
    echo "[Setup] Cleaning up previous run..."
    bash "$SCRIPT_DIR/teardown.sh" 2>/dev/null || true
fi
> "$PIDS_FILE"

# ── 1. Start infrastructure (etcd + Redis) ──
echo ""
echo "[Step 1/6] Starting infrastructure (etcd + Redis)..."
cd "$COMPOSE_DIR"

if docker compose ps --status running 2>/dev/null | grep -q etcd1; then
    echo "  Infrastructure already running, skipping..."
else
    docker compose --profile infra up -d
fi

# Wait for etcd via HTTP API
echo "  Waiting for etcd..."
for i in $(seq 1 60); do
    if curl -sf http://127.0.0.1:2379/health 2>/dev/null | grep -q '"health":"true"'; then
        break
    fi
    sleep 1
done

# Verify etcd
if curl -sf http://127.0.0.1:2379/health 2>/dev/null | grep -q '"health":"true"'; then
    echo "  etcd: OK"
else
    echo "  ERROR: etcd is not healthy after 60s"
    exit 1
fi

# Wait for Redis via docker exec
echo "  Waiting for Redis..."
for i in $(seq 1 30); do
    if docker exec "$REDIS_CONTAINER" redis-cli ping 2>/dev/null | grep -q PONG; then
        break
    fi
    sleep 1
done

if docker exec "$REDIS_CONTAINER" redis-cli ping 2>/dev/null | grep -q PONG; then
    echo "  Redis: OK"
else
    echo "  Redis: not available (CC distributed sync will be disabled)"
fi

cd "$PROJECT_DIR"

# ── 2. Start Python backend servers ──
echo ""
echo "[Step 2/6] Starting Python backend servers..."

for port in 8081 8082 8083; do
    fuser -k "$port/tcp" 2>/dev/null || true
done
sleep 0.3

python3 "$SCRIPT_DIR/backends/server.py" 8081 8082 8083 &
BACKEND_PID=$!
echo "$BACKEND_PID" >> "$PIDS_FILE"
echo "  Backends PID: $BACKEND_PID (ports: 8081, 8082, 8083)"

sleep 1
for port in 8081 8082 8083; do
    if curl -sf -o /dev/null "http://127.0.0.1:$port/health" 2>/dev/null; then
        echo "  Backend :$port OK"
    else
        echo "  ERROR: Backend :$port not responding"
        exit 1
    fi
done

# ── 3. Load global config into etcd ──
echo ""
echo "[Step 3/6] Loading global config into etcd..."
bash "$SCRIPT_DIR/configs/global.sh"

# ── 4. Load site configs into etcd ──
echo ""
echo "[Step 4/6] Loading site configs into etcd..."
bash "$SCRIPT_DIR/configs/load_all.sh"

# ── 5. Build CDN proxy ──
echo ""
echo "[Step 5/6] Building CDN proxy..."
cd "$PROJECT_DIR"
cargo build --release -p cdn-proxy 2>&1 | tail -5
echo "  Build complete"

# ── 6. Start CDN proxy ──
echo ""
echo "[Step 6/6] Starting CDN proxy..."

fuser -k 6188/tcp 2>/dev/null || true
fuser -k 6190/tcp 2>/dev/null || true
sleep 0.5

export CDN_NODE_ID=test-node-01
export CDN_NODE_LABELS=""
export CDN_ENV=development
export CDN_ETCD_ENDPOINTS=http://127.0.0.1:2379
export CDN_ETCD_PREFIX=/nozdormu
export CDN_GEOIP_PATH=/opt/geo
export CDN_CERT_PATH=/tmp/nozdormu-certs
export CDN_LOG_PATH=/tmp/nozdormu-logs
export CDN_CC_CHALLENGE_SECRET=test_e2e_secret_key_12345
export CDN_REDIS_MODE=standalone
export CDN_REDIS_HOST=127.0.0.1
export CDN_REDIS_PORT=6379
export CDN_TRUSTED_PROXIES=127.0.0.0/8
export CDN_LOG_PUSH_REDIS=false
export CDN_COMPRESSION_ENABLED=false
export RUST_LOG=info

mkdir -p /tmp/nozdormu-certs /tmp/nozdormu-logs

"$PROJECT_DIR/target/release/cdn-proxy" -c "$PROJECT_DIR/config/test.yaml" &
PROXY_PID=$!
echo "$PROXY_PID" >> "$PIDS_FILE"
echo "  Proxy PID: $PROXY_PID"

echo "  Waiting for proxy to be ready..."
for i in $(seq 1 30); do
    if curl -sf -o /dev/null "http://127.0.0.1:6188/health" 2>/dev/null; then
        break
    fi
    sleep 1
done

if curl -sf -o /dev/null "http://127.0.0.1:6188/health" 2>/dev/null; then
    echo "  Proxy: OK (http://127.0.0.1:6188)"
else
    echo "  ERROR: Proxy not responding on :6188"
    echo "  Check logs: /tmp/nozdormu-test-error.log"
    exit 1
fi

if curl -sf -o /dev/null "http://127.0.0.1:6190/metrics" 2>/dev/null; then
    echo "  Metrics: OK (http://127.0.0.1:6190)"
fi

echo ""
echo "============================================================"
echo " Setup complete! Run tests with:"
echo "   bash tests/e2e/run_tests.sh"
echo "============================================================"
