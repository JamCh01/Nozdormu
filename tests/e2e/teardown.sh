#!/usr/bin/env bash
# ============================================================
# Nozdormu CDN — E2E Test Teardown
# ============================================================
# Stops all processes started by setup.sh.
# Does NOT stop docker-compose infra (use --all to include it).
#
# Usage:
#   bash tests/e2e/teardown.sh         # Stop proxy + backends only
#   bash tests/e2e/teardown.sh --all   # Also stop docker-compose infra
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PIDS_FILE="$SCRIPT_DIR/.pids"
STOP_INFRA=false

if [[ "${1:-}" == "--all" ]]; then
    STOP_INFRA=true
fi

echo "============================================================"
echo " Nozdormu CDN — E2E Test Teardown"
echo "============================================================"

# ── 1. Kill processes from PID file ──
if [[ -f "$PIDS_FILE" ]]; then
    echo "[Teardown] Stopping processes from PID file..."
    while read -r pid; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            echo "  Killing PID $pid..."
            kill "$pid" 2>/dev/null || true
            # Wait up to 5 seconds for graceful shutdown
            for i in $(seq 1 5); do
                if ! kill -0 "$pid" 2>/dev/null; then
                    break
                fi
                sleep 1
            done
            # Force kill if still running
            if kill -0 "$pid" 2>/dev/null; then
                kill -9 "$pid" 2>/dev/null || true
            fi
        fi
    done < "$PIDS_FILE"
    rm -f "$PIDS_FILE"
else
    echo "[Teardown] No PID file found"
fi

# ── 2. Kill any remaining processes on test ports ──
echo "[Teardown] Cleaning up test ports..."
for port in 6188 8081 8082 8083; do
    if fuser "$port/tcp" 2>/dev/null; then
        echo "  Killing process on port $port..."
        fuser -k "$port/tcp" 2>/dev/null || true
    fi
done

# ── 3. Clean up temp files ──
echo "[Teardown] Cleaning up temp files..."
rm -f /tmp/nozdormu-test.pid
rm -f /tmp/nozdormu-test-error.log
rm -f /tmp/nozdormu-test.sock

# ── 4. Stop docker-compose infra (optional) ──
if $STOP_INFRA; then
    echo "[Teardown] Stopping docker-compose infrastructure..."
    cd "$PROJECT_DIR/docker"
    docker compose --profile infra down
    cd "$PROJECT_DIR"
fi

echo ""
echo "[Teardown] Done"
