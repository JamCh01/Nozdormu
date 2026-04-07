# Plan: Fix All Code Review Issues (CRITICAL + HIGH + MEDIUM)

## Context

Code review found 45 issues (10 CRITICAL, 16 HIGH, 14 MEDIUM, 5 LOW). This plan addresses all CRITICAL and HIGH issues, plus selected MEDIUM issues that are quick wins. LOW issues are deferred.

## Batch 1: CRITICAL Fixes

### C1. Registration allows admin self-signup
**File:** `netpulse/modules/auth/service.py:34`
**Fix:** Hardcode `role=UserRole.SUBSCRIBER` in `register_user`, ignore `data.role`.

### C2. CORS wildcard + credentials
**File:** `netpulse/main.py:83-89`, `netpulse/core/config.py`
**Fix:** Add `ALLOWED_ORIGINS: list[str] = ["http://localhost:3000"]` to Settings. Use `settings.ALLOWED_ORIGINS` in CORS middleware. Remove `allow_credentials=True` when origins is `["*"]`.

### C3. Refresh token fallback skips is_active check
**File:** `netpulse/modules/auth/service.py:65-88`
**Fix:** Make `session` required (remove `| None = None`). Remove the `else` branch entirely.

### C4. Shell injection in install command
**File:** `netpulse/modules/agents/router.py:35-48`
**Fix:** Use `shlex.quote()` for all interpolated values in the bash command. For PowerShell, escape single quotes.

### C5. APScheduler jobs never execute
**File:** `netpulse/worker/main.py:38-73`
**Fix:** Worker needs `session_factory` and `nats_client` injected. Each job becomes an async wrapper that creates a session and calls the real function. `create_worker` and `main.py` updated to pass `session_factory`.

### C6. Alert consumer never subscribed to NATS
**File:** `netpulse/worker/main.py:97-101`
**Fix:** Add a second JetStream subscription for `Subjects.ALERT_EVENTS` in `Worker.start()`. The callback creates a session, calls `handle_alert_event`, commits, and ACKs.

### C7. Buffer race condition
**File:** `netpulse/worker/consumers/buffer.py`
**Fix:** Add `asyncio.Lock`. Acquire in `flush()` — snapshot + clear inside lock, write + ACK outside. On write failure, restore buffer.

### C8. Alert evaluation missing is_deleted filter
**File:** `netpulse/worker/jobs/alert_evaluation.py:94`
**Fix:** Add `AlertRule.is_deleted.is_(False)` to the WHERE clause.

### C9. SSRF via webhook URL
**File:** `netpulse/modules/webhooks/service.py` (create + update)
**Fix:** Add `_validate_webhook_url()` that resolves hostname and rejects private/loopback/link-local IPs. Call on create and update.

### C10. NATS token not persisted (design issue)
**Fix:** Document as known limitation. The NATS token is a placeholder for future NATS auth integration. Add a comment in `auth_service.py`. Not a code fix — architecture decision needed separately.

## Batch 2: HIGH Fixes

### H1. Production code imports unittest.mock
**Files:** `agents/router.py:63-65,79-81`, `monitoring/router.py:25-26,42-43,49-50`
**Fix:** Raise `RuntimeError` if dependency is None instead of yielding AsyncMock.

### H2. Unrestricted setattr in update services
**Files:** `users/service.py:43-45`, `agents/service.py:60-65`, `tasks/service.py:55-62`, `alerting/service.py:60-63`, `webhooks/service.py:67-72`
**Fix:** Add explicit `ALLOWED_FIELDS` set in each update function. Only setattr for allowed fields.

### H3. update_user missing uniqueness check
**File:** `users/service.py:36-48`
**Fix:** Wrap `session.flush()` in try/except `IntegrityError` → raise `ConflictError`.

### H4. Bare except in dependencies
**File:** `core/dependencies.py:17-18,33-34,51-52`
**Fix:** Catch `jwt.InvalidTokenError` specifically. Also refactor to shared `_extract_payload` helper to eliminate duplication.

### H5. Bare except in auth service refresh
**File:** `auth/service.py:69`
**Fix:** Catch `jwt.InvalidTokenError`.

### H6. Login timing side-channel
**File:** `auth/service.py:47-54`
**Fix:** Check `is_active` before `verify_password`. Add dummy bcrypt check when user not found.

### H7. Users list no pagination upper bound
**File:** `users/router.py:27-28`
**Fix:** Add `le=100` to limit parameter.

### H8. NATS connection leak in _notify_agents
**File:** `tasks/router.py:24-45`
**Fix:** Use `try/finally` to ensure `nc.close()`.

### H9. AlertRuleUpdate missing m/n cross-validation
**File:** `alerting/service.py:60-63`
**Fix:** After applying partial updates, check `rule.m_count > rule.n_count` → raise error.

### H10. _row_to_datapoint accesses non-existent _day_timestamp
**File:** `monitoring/service.py:65`
**Fix:** Use `datetime.combine(row.day_ts, datetime.min.time(), tzinfo=UTC).timestamp()`.

### H11. No periodic buffer flush
**File:** `worker/main.py`
**Fix:** Add `asyncio.create_task(_periodic_flush())` in `Worker.start()`. Cancel in `shutdown()`.

### H12. VictoriaClient creates new httpx client per request
**File:** `core/victoria.py`
**Fix:** Create `httpx.AsyncClient` in `__init__`, add `close()` method. Update `main.py` to close on shutdown.

### H13. Duplicate VictoriaClient in worker
**File:** `main.py:55,61`
**Fix:** Pass `vm_client` to `create_worker()` instead of `vm_url`.

### H14. PromQL injection
**File:** `alert_evaluation.py:61`, `probe_consumer.py:35`
**Fix:** Validate UUID format with regex before interpolation.

### H15. Webhook custom_headers bypass in sender
**File:** `webhooks/sender.py:133-136`
**Fix:** Filter reserved headers in sender (defense-in-depth).

### H16. Duplicate AlertEvent every minute
**File:** `alert_evaluation.py:108-159`
**Fix:** Check for existing FIRING event before creating new one.

### H17. Agent heartbeat/tasks endpoints unauthenticated
**Fix:** Document as known design decision — agents use AccessKey for /register, then NATS for data. HTTP endpoints are intentionally lightweight. Add TODO comment for future access-key verification.

## Batch 3: Selected MEDIUM Fixes (quick wins)

### M1. UserUpdate missing field validation → add min_length/max_length
### M2. Alert rules list missing pagination → add skip/limit
### M3. get_tasks_for_agent not filtering Task.is_active → add filter
### M4. List queries missing ORDER BY → add order_by(created_at.desc())
### M5. _StreamDef mutable list → change to tuple
### M6. Jitter calculation negative → use abs()
### M7. ForbiddenError for retry semantics → use BadRequestError (add if not exists)

## Files Modified (summary)

| File | Changes |
|:-----|:--------|
| `core/config.py` | Add ALLOWED_ORIGINS |
| `core/dependencies.py` | Refactor to shared helper, catch jwt.InvalidTokenError |
| `core/victoria.py` | Persistent httpx client |
| `core/nats.py` | tuple instead of list |
| `main.py` | CORS from config, pass vm_client to worker, close vm_client on shutdown |
| `modules/auth/service.py` | Fix register role, fix refresh session required, fix login timing |
| `modules/users/service.py` | Allowlist setattr, catch IntegrityError |
| `modules/users/router.py` | Pagination upper bound |
| `modules/users/schemas.py` | UserUpdate field validation |
| `modules/agents/router.py` | shlex.quote in install cmd, remove mock fallback |
| `modules/agents/service.py` | Allowlist setattr |
| `modules/tasks/router.py` | NATS connection try/finally |
| `modules/tasks/service.py` | Allowlist setattr |
| `modules/tasks/assignment_service.py` | Filter Task.is_active |
| `modules/alerting/router.py` | Add pagination |
| `modules/alerting/service.py` | Allowlist setattr, m/n validation |
| `modules/alerting/schemas.py` | (no change needed — AlertRuleUpdate already defined) |
| `modules/webhooks/service.py` | SSRF validation, allowlist setattr |
| `modules/webhooks/sender.py` | Filter reserved headers |
| `modules/monitoring/service.py` | Fix _day_timestamp |
| `worker/main.py` | Fix jobs, add alert subscription, periodic flush, accept session_factory+vm_client |
| `worker/consumers/buffer.py` | Add asyncio.Lock |
| `worker/consumers/probe_consumer.py` | UUID validation |
| `worker/jobs/alert_evaluation.py` | is_deleted filter, UUID validation, dedup firing events, abs(jitter) |

## Verification

```bash
uv run ruff format . && uv run ruff check --fix . && uv run pytest
```

All existing tests must pass. New behavior verified by updated/new tests where applicable.
