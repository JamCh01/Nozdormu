# Plan: Adapt Agent for Backend Rate Limiting, NATS Auth, and Health Check

## Context

The backend has added API rate limiting (429 responses), real NATS token authentication, and a deep health check endpoint. The agent needs to handle these gracefully. There are 4 changes; 3 require code modifications, 1 is awareness-only (agent_uuid in probe data — already correct).

## Changes

### 1. HTTP 429 Handling in Registration (`src/api/register.rs`)

Add 429-specific detection inside the existing retry loop. Currently lines 25-33 check for 401/403 then fall through to a generic warn. Change to:
- After the 401/403 check, add a 429 check that logs `"Registration rate-limited (429)"` distinctly
- Both 429 and other non-success statuses fall through to the existing backoff retry
- No new error variant needed — 429 is just another retryable status

### 2. HTTP 429 Handling in Heartbeat (`src/api/heartbeat.rs`)

On 429, return `Ok(())` instead of an error — skip this heartbeat cycle silently:
- Extract `status` before the success check
- Add `else if status == TOO_MANY_REQUESTS` branch returning `Ok(())` with a warn log
- Non-429 errors still return `Err(HeartbeatFailed)` as before

### 3. NATS Conditional Token (`src/nats/mod.rs`)

Make `.token()` conditional on non-empty string:
- Build `ConnectOptions` without `.token()` in the chain
- After the builder chain, conditionally call `opts = opts.token(...)` only if `nats_token` is non-empty
- This supports both auth-required (production) and no-auth (dev) NATS servers
- Verified: `async_nats::ConnectOptions::token()` takes `self` by value, returns `Self`

### 4. Health Check Before Registration (`src/api/health.rs` — new file)

Add a non-blocking health pre-flight check:
- New `src/api/health.rs` with `check_health(client, server_url) -> bool`
- GET `/health`, parse `{"status": "ok|degraded"}`, log result
- Returns `true` if server responded with 2xx, `false` otherwise
- Never blocks startup — result is only used for logging
- Called once in `Agent::new()` before registration

### Files to Modify

| File | Change |
|------|--------|
| `src/api/register.rs` | Add 429 detection in retry loop |
| `src/api/heartbeat.rs` | Return Ok(()) on 429 |
| `src/nats/mod.rs` | Conditional `.token()` call |
| `src/api/health.rs` | **New file** — health check function |
| `src/api/mod.rs` | Add `pub mod health;` |
| `src/agent.rs` | Call `check_health()` before registration |

### Tests (using existing `wiremock` dev-dependency)

- `register.rs`: test 429 is retried (not treated as auth failure), test eventual success after 429s
- `heartbeat.rs`: test 429 returns `Ok(())`, test 500 still returns `Err`
- `health.rs`: test ok/degraded/503/network-error cases

### Implementation Order

1. `src/api/heartbeat.rs` — 429 handling
2. `src/api/register.rs` — 429 handling
3. `src/nats/mod.rs` — conditional token
4. `src/api/health.rs` + `src/api/mod.rs` — new health check
5. `src/agent.rs` — call health check
6. Tests for all changes
7. `cargo test` + `cargo clippy` to verify

### Verification

```bash
cargo test          # all existing + new tests pass
cargo clippy        # no warnings
cargo build         # compiles cleanly
```
