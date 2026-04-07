# CDN Edge Node 10-Scenario Integration Test Plan

## Context

User wants to run 10 isolated integration tests on the production VPS (103.141.183.132) covering HTTP/HTTPS proxy, WebSocket/WSS, SSE, HTTP→HTTPS redirect, WS→WSS redirect, and gRPC L4/L7. Each test uses a dedicated `*.test.local` domain with `Host` header injection (no real DNS needed). Self-signed default cert for HTTPS tests.

**Server Info:**
- VPS: 103.141.183.132, NODE_GROUP=TYO, NODE_ID=103-141-183-132
- Redis password: jam114514, Sentinel on 26379/26380/26381
- OpenResty running on 80/443
- Test backends NOT running yet (need to start)

## Plan

### Step 1: Start test backends on VPS

Run via SSH on the VPS:

```bash
# Terminal 1: HTTP+SSE (8081) + WebSocket (8082)
cd /usr/local/openresty/nginx
nohup python3 scripts/test_backend.py > /tmp/test_backend.log 2>&1 &

# Terminal 2: gRPC (8083)
nohup python3 scripts/test_grpc.py server > /tmp/test_grpc.log 2>&1 &
```

Verify backends:
```bash
curl http://127.0.0.1:8081/health   # should return JSON
curl http://127.0.0.1:8082/         # WebSocket server (will fail with HTTP but confirms port open)
```

### Step 2: Write 10 isolated site configs to Redis

Hash key: `cdn:site:TYO:103-141-183-132`

| Site ID | Domain | Test | Origins | Key Config |
|---------|--------|------|---------|------------|
| 801 | `t1-http.test.local` | HTTP proxy | 127.0.0.1:8081 HTTP | force_https=false, cache=off |
| 802 | `t2-https.test.local` | HTTPS proxy | 127.0.0.1:8081 HTTP | force_https=false, cache=off, ssl=on |
| 803 | `t3-ws.test.local` | WS proxy | 127.0.0.1:8082 HTTP | websocket=true, force_https=false |
| 804 | `t4-wss.test.local` | WSS proxy | 127.0.0.1:8082 HTTP | websocket=true, force_https=false, ssl=on |
| 805 | `t5-sse.test.local` | SSE over HTTP | 127.0.0.1:8081 HTTP | sse=true, force_https=false |
| 806 | `t6-sse-ssl.test.local` | SSE over HTTPS | 127.0.0.1:8081 HTTP | sse=true, force_https=false, ssl=on |
| 807 | `t7-redirect.test.local` | HTTP→HTTPS redirect | 127.0.0.1:8081 HTTP | force_https=true, redirect_code=301 |
| 808 | `t8-ws-redirect.test.local` | WS→WSS redirect | 127.0.0.1:8082 HTTP | websocket=true, force_https=true |
| 809 | `t9-grpc-l4.test.local` | gRPC L4 | 127.0.0.1:8083 | stream module, port 50051 |
| 810 | `t10-grpc-l7.test.local` | gRPC L7 | 127.0.0.1:8083 HTTP | grpc.enabled=true, mode=layer7 |

### Step 3: Create init script `scripts/init_test_sites.sh`

This script:
1. Writes all 10 site configs to Redis HASH
2. Writes gRPC L4 stream route config for site 809
3. Publishes reload_all to config channel
4. Calls admin /reload endpoint

### Step 4: Create test script `scripts/run_10_tests.sh`

Each test is a self-contained section:

**Test 1: HTTP Proxy** — `curl -H "Host: t1-http.test.local" http://127.0.0.1/health`
- Expect: HTTP 200, JSON body with "status":"ok"

**Test 2: HTTPS Proxy** — `curl -k -H "Host: t2-https.test.local" https://127.0.0.1/health`
- Expect: HTTP 200, JSON body

**Test 3: WS Proxy** — Python websocket-client to `ws://127.0.0.1/ws` with Host: t3-ws.test.local
- Expect: 101 upgrade, welcome message, echo works

**Test 4: WSS Proxy** — Python websocket-client to `wss://127.0.0.1/ws` with Host: t4-wss.test.local (sslopt no verify)
- Expect: 101 upgrade over TLS, welcome, echo

**Test 5: SSE over HTTP** — `curl -N -H "Host: t5-sse.test.local" -H "Accept: text/event-stream" http://127.0.0.1/sse`
- Expect: Content-Type: text/event-stream, event: connected, event: heartbeat

**Test 6: SSE over HTTPS** — `curl -k -N -H "Host: t6-sse-ssl.test.local" -H "Accept: text/event-stream" https://127.0.0.1/sse`
- Expect: Same as test 5 but over TLS

**Test 7: HTTP→HTTPS Redirect** — `curl -H "Host: t7-redirect.test.local" http://127.0.0.1/some/path?q=1`
- Expect: 301, Location: https://t7-redirect.test.local/some/path?q=1

**Test 8: WS→WSS Redirect** — `curl -H "Host: t8-ws-redirect.test.local" -H "Upgrade: websocket" -H "Connection: Upgrade" -H "Sec-WebSocket-Key: ..." -H "Sec-WebSocket-Version: 13" http://127.0.0.1/ws`
- Expect: 301 redirect to HTTPS (force_https intercepts before WebSocket upgrade)

**Test 9: gRPC L4** — Stream module SNI routing on port 50051
- Write stream route config to Redis: `cdn:stream:TYO:103-141-183-132:routes`
- Test: `python3 scripts/test_grpc.py direct 127.0.0.1:8083` (direct first to verify backend)
- Then test through port 50051 with SNI

**Test 10: gRPC L7** — HTTP/2 gRPC through nginx port 443
- `curl -k -X POST -H "Host: t10-grpc-l7.test.local" -H "Content-Type: application/grpc" -H "TE: trailers" https://127.0.0.1/test.EchoService/Echo`
- Expect: gRPC routing detected (grpc-status header or 502 indicating route to @grpc location)

### Step 5: Cleanup script

Delete all test site configs (801-810) from Redis and reload.

## Files to Create/Modify

1. **`scripts/init_test_sites.sh`** — NEW: Write 10 site configs + stream route to Redis
2. **`scripts/run_10_tests.sh`** — NEW: Run all 10 tests with pass/fail reporting
3. **`scripts/cleanup_test_sites.sh`** — NEW: Remove test data

## Verification

1. SSH to VPS
2. Start backends: `python3 scripts/test_backend.py &` and `python3 scripts/test_grpc.py server &`
3. Run: `bash scripts/init_test_sites.sh`
4. Run: `bash scripts/run_10_tests.sh`
5. Run: `bash scripts/cleanup_test_sites.sh`
6. Expected: 10/10 tests pass
