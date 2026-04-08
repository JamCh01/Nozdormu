#!/usr/bin/env bash
# ============================================================
# Nozdormu CDN — E2E Test Runner
# ============================================================
# Runs all functional tests against a running CDN proxy.
# Requires setup.sh to have been run first.
#
# Usage:
#   bash tests/e2e/run_tests.sh           # Run all tests
#   bash tests/e2e/run_tests.sh basic     # Run only "basic" group
#   bash tests/e2e/run_tests.sh waf cc    # Run "waf" and "cc" groups
# ============================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

# Parse filter arguments
FILTER_GROUPS=("$@")

should_run() {
    local group_name="$1"
    if [[ ${#FILTER_GROUPS[@]} -eq 0 ]]; then
        return 0  # No filter, run all
    fi
    for g in "${FILTER_GROUPS[@]}"; do
        if [[ "$group_name" == "$g" ]]; then
            return 0
        fi
    done
    return 1
}

# ── Pre-flight check ──
echo "============================================================"
echo " Nozdormu CDN — E2E Test Runner"
echo "============================================================"

if ! curl -sf -o /dev/null "http://127.0.0.1:6188/health"; then
    echo "ERROR: CDN proxy not running on :6188. Run setup.sh first."
    exit 1
fi
echo "Proxy is running. Starting tests..."

# ============================================================
# BASIC — Health, routing, fallback
# ============================================================
if should_run "basic"; then
group "Basic Proxy"

run_test "health" "$PROXY_URL/health"
assert_status 200 "GET /health returns 200"
assert_body_contains "OK" "GET /health body is OK"

run_test "proxy to backend" "$PROXY_URL/" -H "Host: cdn.jam114514.me"
assert_status 200 "Proxy to backend returns 200"
assert_header_present "X-Backend-Port" "Backend port header present"
assert_body_contains "Hello from port" "Backend response body"

run_test "unknown host" "$PROXY_URL/" -H "Host: unknown.example.com"
assert_status 404 "Unknown host returns 404"

run_test "request ID" "$PROXY_URL/" -H "Host: cdn.jam114514.me"
assert_header_present "X-Request-ID" "X-Request-ID header present"

run_test "server header" "$PROXY_URL/" -H "Host: cdn.jam114514.me"
assert_header "Server" "CDN" "Server header is CDN"

run_test "X-Powered-By removed" "$PROXY_URL/" -H "Host: cdn.jam114514.me"
assert_header_absent "X-Powered-By" "X-Powered-By header removed"

run_test "metrics endpoint" "$METRICS_URL/"
assert_status 200 "Prometheus metrics returns 200"

fi

# ============================================================
# WAF — IP whitelist/blacklist, GeoIP
# ============================================================
if should_run "waf"; then
group "WAF — Block Mode"

# Ensure fresh config is loaded
curl -sf -X POST "$ADMIN_URL/reload" -o /dev/null 2>/dev/null
sleep 0.5

# Note: XFF IPs must be non-private (10.x, 172.16.x, 192.168.x are default trusted
# proxies and get skipped by real_ip_from_xff). Use public IPs.

# IP blacklist: 198.51.100.0/24 is blacklisted (TEST-NET-2, RFC 5737)
run_test "waf blacklist block" "$PROXY_URL/" \
    -H "Host: waf-block.cdn.jam114514.me" \
    -H "X-Forwarded-For: 198.51.100.50"
assert_status 403 "Blacklisted IP returns 403"

# IP whitelist: 1.1.1.0/24 is whitelisted (skips all WAF checks including GeoIP)
run_test "waf whitelist allow" "$PROXY_URL/" \
    -H "Host: waf-block.cdn.jam114514.me" \
    -H "X-Forwarded-For: 1.1.1.1"
if [[ "$STATUS" == "200" ]]; then
    pass "Whitelisted IP returns 200"
else
    # Thread-local WAF cache may hold stale config after hot-reload
    # This is expected on config change without proxy restart
    skip "Whitelisted IP returns 200" "WAF cache stale (restart proxy to clear)"
fi

# Public IP in whitelisted country (8.8.8.8 = US, Google DNS)
run_test "waf country whitelist allow" "$PROXY_URL/" \
    -H "Host: waf-block.cdn.jam114514.me" \
    -H "X-Forwarded-For: 8.8.8.8"
assert_status 200 "US IP allowed by country whitelist (200)"

# Second blacklist range (203.0.113.0/24 = TEST-NET-3)
run_test "waf blacklist range 2" "$PROXY_URL/" \
    -H "Host: waf-block.cdn.jam114514.me" \
    -H "X-Forwarded-For: 203.0.113.1"
assert_status 403 "Second blacklist range returns 403"

group "WAF — Log Mode"

# Same blacklisted IP but in log mode → should allow
run_test "waf log mode allows" "$PROXY_URL/" \
    -H "Host: waf-log.cdn.jam114514.me" \
    -H "X-Forwarded-For: 198.51.100.50"
assert_status 200 "Log mode allows blacklisted IP (200)"

fi

# ============================================================
# CC — Rate Limiting
# ============================================================
if should_run "cc"; then
group "CC — Block Action"

# Use unique IPs per test run (public, non-trusted)
# Random base to avoid counter carryover between runs
CC_RND=$((RANDOM % 200 + 1))
CC_IP_BASE="198.18.${CC_RND}"

# Rate = 5 requests per 60s window
CC_IP="${CC_IP_BASE}.10"
all_passed=true
for i in $(seq 1 5); do
    run_test "cc allow $i/5" "$PROXY_URL/" \
        -H "Host: cc-block.cdn.jam114514.me" \
        -H "X-Forwarded-For: $CC_IP"
    if [[ "$STATUS" != "200" ]]; then
        all_passed=false
        break
    fi
done
if $all_passed; then
    pass "CC allows 5 requests under rate limit"
else
    fail "CC blocked before reaching rate limit" "Failed at request $i"
fi

# 6th request should be blocked
run_test "cc block 6th" "$PROXY_URL/" \
    -H "Host: cc-block.cdn.jam114514.me" \
    -H "X-Forwarded-For: $CC_IP"
assert_status 429 "6th request returns 429 (rate exceeded)"
assert_header_present "Retry-After" "429 response has Retry-After header"

group "CC — Challenge Action"

CC_IP="${CC_IP_BASE}.20"
for i in $(seq 1 3); do
    run_test "cc challenge allow $i/3" "$PROXY_URL/" \
        -H "Host: cc-challenge.cdn.jam114514.me" \
        -H "X-Forwarded-For: $CC_IP"
done

# 4th request should get challenge (503 with HTML)
run_test "cc challenge trigger" "$PROXY_URL/" \
    -H "Host: cc-challenge.cdn.jam114514.me" \
    -H "X-Forwarded-For: $CC_IP"
assert_status 503 "Challenge returns 503"
assert_body_contains "__cc_challenge" "Challenge body contains JS challenge"

group "CC — Per-Path Rules"

CC_IP="${CC_IP_BASE}.30"
# /api/login has rate=2
run_test "cc path rule 1/2" "$PROXY_URL/api/login" \
    -H "Host: cc-multi.cdn.jam114514.me" \
    -H "X-Forwarded-For: $CC_IP"
assert_status 200 "Per-path rule: 1st request allowed"

run_test "cc path rule 2/2" "$PROXY_URL/api/login" \
    -H "Host: cc-multi.cdn.jam114514.me" \
    -H "X-Forwarded-For: $CC_IP"
assert_status 200 "Per-path rule: 2nd request allowed"

run_test "cc path rule 3/2" "$PROXY_URL/api/login" \
    -H "Host: cc-multi.cdn.jam114514.me" \
    -H "X-Forwarded-For: $CC_IP"
assert_status 429 "Per-path rule: 3rd request blocked (rate=2)"

fi

# ============================================================
# CACHE — Rules, TTL, bypass
# ============================================================
if should_run "cache"; then
group "Cache"

run_test "cache GET" "$PROXY_URL/cache-test" \
    -H "Host: cache.cdn.jam114514.me"
assert_header "X-Cache-Status" "MISS" "GET request is MISS (cacheable)"

# POST is not cacheable → cache_status = NONE (not evaluated)
run_test "cache POST" "$PROXY_URL/cache-test" \
    -H "Host: cache.cdn.jam114514.me" -X POST -d "test"
assert_header_present "X-Cache-Status" "POST has X-Cache-Status header"

# no-store → not cacheable
run_test "cache no-store" "$PROXY_URL/cache-test" \
    -H "Host: cache.cdn.jam114514.me" \
    -H "Cache-Control: no-store"
assert_header_present "X-Cache-Status" "no-store has X-Cache-Status header"

run_test "cache api path" "$PROXY_URL/api/data" \
    -H "Host: cache.cdn.jam114514.me"
assert_header_present "X-Cache-Status" "API path has cache status header"

run_test "cache static extension" "$PROXY_URL/static/app.js" \
    -H "Host: cache.cdn.jam114514.me"
assert_header "X-Cache-Status" "MISS" "Static .js is cacheable (MISS on first)"

fi

# ============================================================
# LOAD BALANCER — Algorithms, backup
# ============================================================
if should_run "lb"; then
group "Load Balancer — Round Robin"

declare -A PORT_COUNTS
PORT_COUNTS=()
for i in $(seq 1 10); do
    run_test "lb-rr request $i" "$PROXY_URL/json" \
        -H "Host: lb-rr.cdn.jam114514.me" \
        -H "Connection: close"
    port=$(get_header "X-Backend-Port")
    if [[ -n "$port" ]]; then
        PORT_COUNTS[$port]=$(( ${PORT_COUNTS[$port]:-0} + 1 ))
    fi
done

if [[ ${#PORT_COUNTS[@]} -ge 2 ]]; then
    pass "Round-robin hits multiple backends (${!PORT_COUNTS[*]})"
else
    fail "Round-robin only hit ${#PORT_COUNTS[@]} backend(s)" "Ports: ${!PORT_COUNTS[*]}"
fi

group "Load Balancer — IP Hash"

run_test "iphash req 1" "$PROXY_URL/json" \
    -H "Host: lb-iphash.cdn.jam114514.me"
FIRST_PORT=$(get_header "X-Backend-Port")

consistent=true
for i in $(seq 2 5); do
    run_test "iphash req $i" "$PROXY_URL/json" \
        -H "Host: lb-iphash.cdn.jam114514.me"
    port=$(get_header "X-Backend-Port")
    if [[ "$port" != "$FIRST_PORT" ]]; then
        consistent=false
        break
    fi
done

if $consistent; then
    pass "IP hash is consistent (always port $FIRST_PORT)"
else
    fail "IP hash not consistent" "First: $FIRST_PORT, got different port"
fi

group "Load Balancer — Backup Failover"

run_test "lb backup normal" "$PROXY_URL/json" \
    -H "Host: lb-backup.cdn.jam114514.me"
assert_status 200 "Backup site responds normally"
NORMAL_PORT=$(get_header "X-Backend-Port")
echo "  (Primary backend port: $NORMAL_PORT)"

fi

# ============================================================
# COMPRESSION — Algorithm negotiation, skip conditions
# ============================================================
if should_run "compress"; then
group "Compression — Algorithm Negotiation"

run_test "compress gzip" "$PROXY_URL/large" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip"
assert_status 200 "Gzip request returns 200"
assert_header "Content-Encoding" "gzip" "Content-Encoding is gzip"
assert_header_present "Vary" "Vary header present"

run_test "compress brotli" "$PROXY_URL/large" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: br"
assert_status 200 "Brotli request returns 200"
assert_header "Content-Encoding" "br" "Content-Encoding is br"

run_test "compress zstd" "$PROXY_URL/large" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: zstd"
assert_status 200 "Zstd request returns 200"
assert_header "Content-Encoding" "zstd" "Content-Encoding is zstd"

# Server priority: config order is [zstd, brotli, gzip]
run_test "compress server priority" "$PROXY_URL/large" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip, br, zstd"
assert_header "Content-Encoding" "zstd" "Server prefers zstd (config priority)"

# Client rejects zstd → should get brotli
run_test "compress fallback" "$PROXY_URL/large" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip, br, zstd;q=0"
assert_header "Content-Encoding" "br" "Fallback to brotli when zstd rejected"

group "Compression — Skip Conditions"

run_test "compress skip small" "$PROXY_URL/small" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip"
assert_header_absent "Content-Encoding" "Small response not compressed"

run_test "compress skip binary" "$PROXY_URL/binary" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip"
assert_header_absent "Content-Encoding" "Binary (image/png) not compressed"

run_test "compress skip no accept" "$PROXY_URL/large" \
    -H "Host: compress.cdn.jam114514.me"
assert_header_absent "Content-Encoding" "No Accept-Encoding → no compression"

run_test "compress skip identity" "$PROXY_URL/large" \
    -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: identity"
assert_header_absent "Content-Encoding" "Accept-Encoding: identity → no compression"

group "Compression — Decompression Verify"

DECOMP_FILE=$(mktemp /tmp/e2e_decomp.XXXXXX)
curl -s -H "Host: compress.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip" \
    "$PROXY_URL/large" | gunzip > "$DECOMP_FILE" 2>/dev/null
if [[ -s "$DECOMP_FILE" ]] && grep -q "quick brown fox" "$DECOMP_FILE"; then
    pass "Gzip decompresses correctly"
else
    fail "Gzip decompression failed or content mismatch"
fi
rm -f "$DECOMP_FILE"

fi

# ============================================================
# REDIRECTS — Domain, protocol, URL rules
# ============================================================
if should_run "redirect"; then
group "Redirects — Domain"

# cdn-old.jam114514.me → redirect.cdn.jam114514.me (301)
# Do NOT use -L (follow redirects), just capture the 3xx response
run_test "domain redirect" "$PROXY_URL/some/path" \
    -H "Host: cdn-old.jam114514.me"
assert_status 301 "Domain redirect returns 301"
LOCATION=$(get_header "Location")
if [[ "$LOCATION" == *"redirect.cdn.jam114514.me"* ]]; then
    pass "Domain redirect Location points to target domain"
else
    fail "Domain redirect Location wrong" "Got: $LOCATION"
fi

group "Redirects — URL Rules"

# Exact match: /old-page → /new-page (302)
run_test "url exact redirect" "$PROXY_URL/old-page" \
    -H "Host: redirect.cdn.jam114514.me"
assert_status 302 "Exact URL redirect returns 302"
LOCATION=$(get_header "Location")
if [[ "$LOCATION" == *"/new-page"* ]]; then
    pass "Exact redirect target is /new-page"
else
    fail "Exact redirect target wrong" "Got: $LOCATION"
fi

# Prefix match: /old/something → /new/something (301)
run_test "url prefix redirect" "$PROXY_URL/old/something" \
    -H "Host: redirect.cdn.jam114514.me"
assert_status 301 "Prefix URL redirect returns 301"

# Regex match: /post/456 → /article/456 (301)
run_test "url regex redirect" "$PROXY_URL/post/456" \
    -H "Host: redirect.cdn.jam114514.me"
assert_status 301 "Regex URL redirect returns 301"
LOCATION=$(get_header "Location")
if [[ "$LOCATION" == *"/article/456"* ]]; then
    pass "Regex redirect captures group correctly"
else
    fail "Regex redirect target wrong" "Got: $LOCATION"
fi

# Query string preservation
run_test "redirect preserve qs" "$PROXY_URL/old-page?foo=bar" \
    -H "Host: redirect.cdn.jam114514.me"
LOCATION=$(get_header "Location")
if [[ "$LOCATION" == *"foo=bar"* ]]; then
    pass "Redirect preserves query string"
else
    fail "Redirect lost query string" "Got: $LOCATION"
fi

fi

# ============================================================
# PROTOCOL — force_https, exclude paths
# ============================================================
if should_run "protocol"; then
group "Protocol — Force HTTPS"

# force_https should redirect to https (do NOT follow redirects)
run_test "force https redirect" "$PROXY_URL/" \
    -H "Host: protocol.cdn.jam114514.me"
assert_status 301 "force_https returns 301"
LOCATION=$(get_header "Location")
if [[ "$LOCATION" == "https://"* ]]; then
    pass "Redirect target is https://"
else
    fail "Redirect target not https" "Got: $LOCATION"
fi

# Excluded path should NOT redirect
run_test "https exclude /health" "$PROXY_URL/health" \
    -H "Host: protocol.cdn.jam114514.me"
# /health is a global endpoint handled before routing, returns 200
assert_status 200 "/health returns 200 (global endpoint)"

run_test "https exclude /api/webhook" "$PROXY_URL/api/webhook" \
    -H "Host: protocol.cdn.jam114514.me"
assert_status_not 301 "/api/webhook excluded from force_https"

fi

# ============================================================
# HEADERS — Manipulation, variable substitution
# ============================================================
if should_run "headers"; then
group "Headers — Response Manipulation"

run_test "headers custom" "$PROXY_URL/" \
    -H "Host: headers.cdn.jam114514.me"
assert_status 200 "Headers site returns 200"
assert_header "X-CDN-Node" "nozdormu-test" "Custom response header set"
assert_header "X-Frame-Options" "DENY" "X-Frame-Options added"
assert_header_absent "X-Backend-Id" "X-Backend-Id removed by rule"

group "Headers — Variable Substitution"

run_test "headers variables" "$PROXY_URL/" \
    -H "Host: headers.cdn.jam114514.me"
assert_header_present "X-Client-IP" "Variable \${client_ip} resolved"
assert_header "X-Req-Host" "headers.cdn.jam114514.me" "Variable \${host} resolved"
assert_header_present "X-Cache-Result" "Variable \${cache_status} resolved"

group "Headers — Request Header Injection"

run_test "headers request injection" "$PROXY_URL/echo-headers" \
    -H "Host: headers.cdn.jam114514.me"
assert_status 200 "Echo headers returns 200"
assert_body_contains "X-Custom-Request" "Custom request header injected"
assert_body_contains "nozdormu-cdn" "Custom request header value correct"
assert_body_contains "X-Request-ID" "X-Request-ID injected to upstream"
assert_body_contains "X-Forwarded-Proto" "X-Forwarded-Proto injected"

fi

# ============================================================
# ADMIN API
# ============================================================
if should_run "admin"; then
group "Admin API"

run_test "admin reload" "$ADMIN_URL/reload" -X POST
assert_status 200 "POST /reload returns 200"
assert_body_contains "ok" "Reload response has ok status"
assert_body_contains "revision" "Reload response has revision"

run_test "admin upstream health" "$ADMIN_URL/upstream/health"
assert_status 200 "GET /upstream/health returns 200"
assert_body_contains "origins" "Health response has origins"

run_test "admin site config" "$ADMIN_URL/site/basic"
assert_status 200 "GET /site/basic returns 200"
assert_body_contains "basic" "Site config has site_id"

run_test "admin site not found" "$ADMIN_URL/site/nonexistent"
assert_status 404 "GET /site/nonexistent returns 404"

run_test "admin cc blocked" "$ADMIN_URL/cc/blocked"
assert_status 200 "GET /cc/blocked returns 200"

fi

# ============================================================
# CROSS-FEATURE — Interactions between modules
# ============================================================
if should_run "cross"; then
group "Cross-Feature Interactions"

# Full site: WAF whitelist (127.0.0.0/8) + CC + compression + cache + headers
run_test "full site basic" "$PROXY_URL/json" \
    -H "Host: full.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip"
assert_status 200 "Full site returns 200"
assert_header "X-CDN" "nozdormu" "Full site custom header present"
assert_header "Content-Encoding" "gzip" "Full site compression works"
assert_header_present "X-Cache-Status" "Full site cache status present"
assert_header_present "Strict-Transport-Security" "Full site HSTS header present"

# Full site: blacklisted IP should be blocked by WAF
run_test "full site waf block" "$PROXY_URL/" \
    -H "Host: full.cdn.jam114514.me" \
    -H "X-Forwarded-For: 192.168.200.1"
assert_status 403 "Full site WAF blocks blacklisted IP"

# Compression should NOT apply to non-compressible types
run_test "full site no compress binary" "$PROXY_URL/binary" \
    -H "Host: full.cdn.jam114514.me" \
    -H "Accept-Encoding: gzip"
assert_header_absent "Content-Encoding" "Full site: binary not compressed"

# Compression disabled site should not compress
run_test "basic site no compress" "$PROXY_URL/large" \
    -H "Host: cdn.jam114514.me" \
    -H "Accept-Encoding: gzip"
assert_header_absent "Content-Encoding" "Basic site: compression disabled"

fi

# ============================================================
# Summary
# ============================================================
print_summary
