#!/usr/bin/env bash
# ============================================================
# Load all site configs into etcd
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${CDN_ETCD_PREFIX:-/nozdormu}"
ETCD_CONTAINER="${ETCD_CONTAINER:-docker-etcd1-1}"

etcdctl() {
    docker exec "$ETCD_CONTAINER" etcdctl "$@"
}

echo "[Sites] Loading site configs into etcd (prefix: $PREFIX)..."

count=0
for config_file in "$SCRIPT_DIR"/site_*.json; do
    if [[ ! -f "$config_file" ]]; then
        continue
    fi

    # Extract site_id from JSON
    site_id=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['site_id'])" "$config_file")
    key="${PREFIX}/sites/${site_id}"

    # Validate JSON
    if ! python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$config_file" 2>/dev/null; then
        echo "  ERROR: Invalid JSON in $config_file"
        continue
    fi

    # Read file content and put into etcd via docker exec
    config_content=$(cat "$config_file")
    etcdctl put "$key" -- "$config_content"
    echo "  Loaded: $site_id ($(basename "$config_file"))"
    count=$((count + 1))
done

echo "[Sites] Done: $count sites loaded"

# Verify
echo ""
echo "[Sites] Verification — keys in etcd:"
etcdctl get "${PREFIX}/sites/" --prefix --keys-only | grep -v '^$' | while read -r key; do
    echo "  $key"
done
