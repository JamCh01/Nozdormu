#!/usr/bin/env bash
# ============================================================
# Nozdormu CDN — E2E Test Library
# ============================================================
# Source this file in test scripts:
#   source "$(dirname "$0")/lib.sh"
# ============================================================

set -euo pipefail

# ── Colors ──
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ── Counters ──
PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
CURRENT_GROUP=""

# ── Configuration ──
PROXY_HOST="${PROXY_HOST:-127.0.0.1}"
PROXY_PORT="${PROXY_PORT:-6188}"
PROXY_URL="http://${PROXY_HOST}:${PROXY_PORT}"
ADMIN_URL="http://127.0.0.1:8080"
METRICS_URL="http://127.0.0.1:6190"
ETCD_ENDPOINT="${ETCD_ENDPOINT:-http://127.0.0.1:2379}"
CURL_TIMEOUT="${CURL_TIMEOUT:-10}"

# Temp file for curl output
RESP_FILE=$(mktemp /tmp/e2e_resp.XXXXXX)
RESP_HEADERS=$(mktemp /tmp/e2e_headers.XXXXXX)
trap 'rm -f "$RESP_FILE" "$RESP_HEADERS"' EXIT

# ── Test Group ──
group() {
    CURRENT_GROUP="$1"
    echo ""
    echo -e "${BOLD}${BLUE}=== $1 ===${NC}"
}

# ── Test Execution ──
# Usage: run_test "test name" curl_args...
# Sets $STATUS, $BODY, and headers in $RESP_HEADERS
run_test() {
    local name="$1"
    shift
    # Reset
    STATUS=0
    BODY=""
    > "$RESP_FILE"
    > "$RESP_HEADERS"

    if ! STATUS=$(curl -s -o "$RESP_FILE" -w "%{http_code}" \
        -D "$RESP_HEADERS" \
        --max-time "$CURL_TIMEOUT" \
        "$@" 2>/dev/null); then
        STATUS=0
    fi
    BODY=$(cat "$RESP_FILE" 2>/dev/null || true)
}

# ── Assertions ──

pass() {
    local name="$1"
    PASS_COUNT=$((PASS_COUNT + 1))
    echo -e "  ${GREEN}PASS${NC} $name"
}

fail() {
    local name="$1"
    local detail="${2:-}"
    FAIL_COUNT=$((FAIL_COUNT + 1))
    echo -e "  ${RED}FAIL${NC} $name"
    if [[ -n "$detail" ]]; then
        echo -e "       ${RED}$detail${NC}"
    fi
}

skip() {
    local name="$1"
    local reason="${2:-}"
    SKIP_COUNT=$((SKIP_COUNT + 1))
    echo -e "  ${YELLOW}SKIP${NC} $name${reason:+ ($reason)}"
}

# assert_status CODE "test name"
assert_status() {
    local expected="$1"
    local name="$2"
    if [[ "$STATUS" == "$expected" ]]; then
        pass "$name"
    else
        fail "$name" "expected status $expected, got $STATUS"
    fi
}

# assert_status_not CODE "test name"
assert_status_not() {
    local unexpected="$1"
    local name="$2"
    if [[ "$STATUS" != "$unexpected" ]]; then
        pass "$name"
    else
        fail "$name" "expected status != $unexpected, got $STATUS"
    fi
}

# assert_header "Header-Name" "expected_value" "test name"
# Case-insensitive header name match, exact value match
assert_header() {
    local header_name="$1"
    local expected_value="$2"
    local name="$3"
    local actual
    actual=$(grep -i "^${header_name}:" "$RESP_HEADERS" | head -1 | sed 's/^[^:]*: *//' | tr -d '\r\n')
    if [[ "$actual" == "$expected_value" ]]; then
        pass "$name"
    else
        fail "$name" "header '$header_name': expected '$expected_value', got '$actual'"
    fi
}

# assert_header_contains "Header-Name" "substring" "test name"
assert_header_contains() {
    local header_name="$1"
    local substring="$2"
    local name="$3"
    local actual
    actual=$(grep -i "^${header_name}:" "$RESP_HEADERS" | head -1 | sed 's/^[^:]*: *//' | tr -d '\r\n')
    if [[ "$actual" == *"$substring"* ]]; then
        pass "$name"
    else
        fail "$name" "header '$header_name' does not contain '$substring' (got: '$actual')"
    fi
}

# assert_header_present "Header-Name" "test name"
assert_header_present() {
    local header_name="$1"
    local name="$2"
    if grep -qi "^${header_name}:" "$RESP_HEADERS"; then
        pass "$name"
    else
        fail "$name" "header '$header_name' not present"
    fi
}

# assert_header_absent "Header-Name" "test name"
assert_header_absent() {
    local header_name="$1"
    local name="$2"
    if ! grep -qi "^${header_name}:" "$RESP_HEADERS"; then
        pass "$name"
    else
        local actual
        actual=$(grep -i "^${header_name}:" "$RESP_HEADERS" | head -1 | tr -d '\r\n')
        fail "$name" "header '$header_name' should be absent (found: '$actual')"
    fi
}

# assert_body_contains "substring" "test name"
assert_body_contains() {
    local substring="$1"
    local name="$2"
    if [[ "$BODY" == *"$substring"* ]]; then
        pass "$name"
    else
        fail "$name" "body does not contain '$substring' (body: ${BODY:0:200})"
    fi
}

# assert_body_not_contains "substring" "test name"
assert_body_not_contains() {
    local substring="$1"
    local name="$2"
    if [[ "$BODY" != *"$substring"* ]]; then
        pass "$name"
    else
        fail "$name" "body should not contain '$substring'"
    fi
}

# assert_body_equals "expected" "test name"
assert_body_equals() {
    local expected="$1"
    local name="$2"
    if [[ "$BODY" == "$expected" ]]; then
        pass "$name"
    else
        fail "$name" "body mismatch (expected: '${expected:0:100}', got: '${BODY:0:100}')"
    fi
}

# get_header "Header-Name" — returns header value
get_header() {
    local header_name="$1"
    grep -i "^${header_name}:" "$RESP_HEADERS" | head -1 | sed 's/^[^:]*: *//' | tr -d '\r\n'
}

# ── Helpers ──

# Wait for a URL to return 200
wait_for_url() {
    local url="$1"
    local max_wait="${2:-30}"
    local i=0
    while [[ $i -lt $max_wait ]]; do
        if curl -sf -o /dev/null --max-time 2 "$url" 2>/dev/null; then
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

# Wait for a TCP port to be open
wait_for_port() {
    local host="$1"
    local port="$2"
    local max_wait="${3:-30}"
    local i=0
    while [[ $i -lt $max_wait ]]; do
        if bash -c "echo >/dev/tcp/$host/$port" 2>/dev/null; then
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

# etcdctl wrapper (via docker exec)
ETCD_CONTAINER="${ETCD_CONTAINER:-docker-etcd1-1}"
etcdctl() {
    docker exec "$ETCD_CONTAINER" etcdctl "$@"
}

# ── Summary ──
print_summary() {
    local total=$((PASS_COUNT + FAIL_COUNT + SKIP_COUNT))
    echo ""
    echo -e "${BOLD}============================================================${NC}"
    echo -e "${BOLD} Test Summary${NC}"
    echo -e "${BOLD}============================================================${NC}"
    echo -e "  ${GREEN}PASS: $PASS_COUNT${NC}"
    echo -e "  ${RED}FAIL: $FAIL_COUNT${NC}"
    echo -e "  ${YELLOW}SKIP: $SKIP_COUNT${NC}"
    echo -e "  Total: $total"
    echo -e "${BOLD}============================================================${NC}"

    if [[ $FAIL_COUNT -gt 0 ]]; then
        echo -e "${RED}${BOLD}SOME TESTS FAILED${NC}"
        return 1
    else
        echo -e "${GREEN}${BOLD}ALL TESTS PASSED${NC}"
        return 0
    fi
}
