# Plan: 5 项生产级功能（限流、登出、审计、Webhook 重试、分页）

## Context

实现 5 项生产级功能：API 限流、JWT Token 撤销/登出、审计日志（PG 表）、Webhook 自动重试、列表接口分页元数据（Envelope 格式）。

---

## Feature 1: API 限流

**方案：** 基于 Redis 的滑动窗口限流，作为 FastAPI 中间件实现。不引入新依赖。

### 1.1 限流核心

**新文件：`netpulse/core/rate_limit.py`**

```python
async def check_rate_limit(redis: Redis, key: str, max_requests: int, window_seconds: int) -> bool:
    """滑动窗口限流。返回 True 表示允许，False 表示超限。"""
    # 使用 Redis INCR + EXPIRE 实现固定窗口（简单高效）
    current = await redis.incr(key)
    if current == 1:
        await redis.expire(key, window_seconds)
    return current <= max_requests
```

### 1.2 限流中间件

**修改：`netpulse/main.py`** — 添加 Starlette 中间件

```python
@app.middleware("http")
async def rate_limit_middleware(request, call_next):
    # 只对特定端点限流
    path = request.url.path
    client_ip = request.client.host
    
    rate_limits = {
        "/api/v1/auth/login": (10, 60),        # 10 次/分钟
        "/api/v1/auth/register": (5, 60),       # 5 次/分钟
        "/api/v1/auth/refresh": (20, 60),       # 20 次/分钟
        "/api/v1/agents/register": (10, 60),    # 10 次/分钟
    }
    # 心跳端点用通配符匹配
    if "/heartbeat" in path:
        rate_limits[path] = (120, 60)           # 120 次/分钟（每 30s 一次心跳 × 多 agent）
    
    config = rate_limits.get(path)
    if config:
        max_req, window = config
        key = f"ratelimit:{path}:{client_ip}"
        allowed = await check_rate_limit(redis_client, key, max_req, window)
        if not allowed:
            return JSONResponse(status_code=429, content={"detail": "Too many requests"})
    
    return await call_next(request)
```

### 1.3 测试

- `tests/core/test_rate_limit.py` — NEW: 测试 check_rate_limit 函数
- `tests/test_rate_limit_middleware.py` — NEW: 测试中间件 429 响应

---

## Feature 2: JWT Token 撤销/登出

**方案：** JWT 加 `jti` claim，登出时将 jti 写入 Redis（TTL = token 剩余有效期），认证依赖检查 Redis 黑名单。

### 2.1 Token 加 jti

**修改：`netpulse/core/security.py`**

```python
import uuid

def create_access_token(...) -> str:
    payload = {
        "sub": user_uuid, "role": role, "type": "access",
        "jti": uuid.uuid4().hex,  # 新增
        "exp": ..., "iat": ...,
    }

def create_refresh_token(...) -> str:
    payload = {
        "sub": user_uuid, "type": "refresh",
        "jti": uuid.uuid4().hex,  # 新增
        "exp": ..., "iat": ...,
    }
```

### 2.2 认证依赖检查黑名单

**修改：`netpulse/core/dependencies.py`**

- 所有 factory 函数新增 `redis` 参数
- `_extract_payload` 新增 `redis` 参数，验证 token 后检查 `redis.exists(f"revoked:{jti}")`
- 如果 jti 在黑名单中，抛出 401

### 2.3 登出端点

**修改：`netpulse/modules/auth/router.py`**

- `create_auth_router` 新增 `redis_client` 参数
- 新增 `POST /api/v1/auth/logout` 端点：
  - 从 Authorization header 提取 access token
  - 解码获取 jti 和 exp
  - 计算剩余 TTL = exp - now
  - `redis.setex(f"revoked:{jti}", ttl, "1")`
  - 如果请求体包含 refresh_token，同样撤销

### 2.4 Refresh Token 轮换

**修改：`netpulse/modules/auth/service.py`**

- `refresh_token()` 新增 `redis` 参数
- 发放新 token 对后，将旧 refresh token 的 jti 写入 Redis 黑名单

### 2.5 主入口串联

**修改：`netpulse/main.py`**

- `create_auth_router(secret, get_session, redis_client)` 传入 redis
- 所有使用 auth 依赖的 router factory 传入 redis_client（users, alerting, webhooks, tasks, agents, monitoring）

### 2.6 测试

- 更新 `tests/core/test_security.py` — jti 存在于 token payload
- `tests/modules/auth/test_logout.py` — NEW: 登出后 token 失效
- 更新 `tests/modules/auth/test_service.py` — refresh 轮换撤销旧 token

---

## Feature 3: 审计日志

**方案：** 新建 `audit_logs` 表，在 service 层关键操作后写入审计记录。提供 Admin 查询 API。

### 3.1 数据模型

**新文件：`netpulse/core/audit.py`**

```python
class AuditLog(Base):
    __tablename__ = "audit_logs"
    
    log_uuid        UUID PK
    actor_uuid      UUID nullable (系统操作为 null)
    actor_role      String(32) nullable
    action          String(64) NOT NULL  # e.g. "agent.create", "task.update", "user.login"
    resource_type   String(64) NOT NULL  # e.g. "agent", "task", "user"
    resource_uuid   UUID nullable
    details         JSONB nullable       # 变更详情（old/new 值等）
    ip_address      String(45) nullable  # IPv4/IPv6
    created_at      DateTime server_default=now()

# 索引：(resource_type, resource_uuid), (actor_uuid), (created_at DESC)
```

**辅助函数：**
```python
async def write_audit_log(session, actor_uuid, actor_role, action, resource_type, resource_uuid=None, details=None, ip_address=None):
    log = AuditLog(...)
    session.add(log)
    # 不 flush — 随主事务一起提交
```

### 3.2 Alembic 迁移

**新文件：`alembic/versions/e5f6a7b8c9d0_add_audit_logs.py`**

### 3.3 Service 层集成

在以下 service 函数中调用 `write_audit_log`：

**关键操作（需要传入 actor_uuid）：**
- `agents/service.py`: create_agent, update_agent, disable_agent
- `tasks/service.py`: create_task, update_task, deactivate_task
- `alerting/service.py`: create_alert_rule, update_alert_rule, delete_alert_rule
- `webhooks/service.py`: create_webhook, update_webhook, delete_webhook, rotate_secret
- `users/service.py`: update_user, disable_user, change_password, create_group, update_group, delete_group, add_user_to_group, remove_user_from_group
- `auth/service.py`: register_user, login (成功/失败), refresh_token

**Router 层变更：** 需要将 `user["sub"]` 和 `request.client.host` 传入 service 函数。对于已有 user 参数的端点（alerting, webhooks），直接传递。对于没有的（agents, tasks），需要新增参数。

### 3.4 查询 API

**新文件：`netpulse/core/audit_router.py`** 或在 main.py 中直接添加

```
GET /api/v1/audit/logs    Admin — 列表（支持 ?actor_uuid=&resource_type=&action=&skip=&limit=）
```

### 3.5 测试

- `tests/core/test_audit.py` — NEW: write_audit_log 测试
- `tests/test_audit_router.py` — NEW: 审计日志查询端点测试

---

## Feature 4: Webhook 自动重试

**方案：** APScheduler 定时任务，每分钟扫描失败的 delivery，按指数退避策略重试。

### 4.1 重试策略

退避间隔：1min, 5min, 30min, 2h, 12h（共 5 次重试）
```python
RETRY_BACKOFFS = [60, 300, 1800, 7200, 43200]  # 秒
MAX_RETRIES = 5
```

### 4.2 WebhookDelivery 模型变更

**修改：`netpulse/modules/webhooks/models.py`**

```python
next_retry_at: DateTime(timezone=True) nullable  # 下次重试时间
```

### 4.3 Alembic 迁移

**新文件：`alembic/versions/f6a7b8c9d0e1_add_webhook_retry_fields.py`**

- `webhook_deliveries` 表新增 `next_retry_at` (DateTime, nullable)

### 4.4 Sender 变更

**修改：`netpulse/modules/webhooks/sender.py`**

- `deliver_webhook` 失败时，如果 `attempt < MAX_RETRIES`，计算 `next_retry_at` 并写入 delivery 记录

### 4.5 重试 Worker Job

**新文件：`netpulse/worker/jobs/webhook_retry.py`**

```python
async def retry_failed_webhooks(session) -> int:
    """查询所有 status=FAILED 且 next_retry_at <= now 的 delivery，重试。"""
    # 1. 查询待重试的 delivery（JOIN webhook 确保 is_active 且 !is_deleted）
    # 2. 对每个 delivery：反序列化 request_body，调用 deliver_webhook(attempt+1)
    # 3. 返回重试数量
```

**修改：`netpulse/worker/main.py`**

- 注册新 job：`webhook_retry`，interval=1 minute

### 4.6 测试

- `tests/worker/jobs/test_webhook_retry.py` — NEW: 重试逻辑测试
- 更新 `tests/modules/webhooks/test_sender.py` — 失败时设置 next_retry_at

---

## Feature 5: 列表接口分页元数据

**方案：** 统一 Envelope 格式 `{"items": [...], "total": N, "skip": M, "limit": L}`。

### 5.1 通用分页 Schema

**新文件：`netpulse/core/pagination.py`**

```python
from pydantic import BaseModel
from typing import Generic, TypeVar

T = TypeVar("T")

class PaginatedResponse(BaseModel, Generic[T]):
    items: list[T]
    total: int
    skip: int
    limit: int
```

### 5.2 Service 层变更

所有 list 函数新增 `count` 查询，返回 `(items, total)` 元组：

- `agents/service.py`: list_agents → 返回 (agents, total)
- `tasks/service.py`: list_tasks → 返回 (tasks, total)
- `users/service.py`: list_users, list_groups → 返回 (items, total)
- `alerting/service.py`: list_alert_rules, list_alert_events → 返回 (items, total)
- `webhooks/service.py`: list_webhooks, list_deliveries → 返回 (items, total)

### 5.3 Router 层变更

所有 list 端点改为返回 Envelope 格式：

```python
@router.get("/", response_model=PaginatedResponse[UserResponse])
async def list_users_route(skip, limit, ...):
    items, total = await list_users(session, skip=skip, limit=limit)
    return PaginatedResponse(items=items, total=total, skip=skip, limit=limit)
```

**受影响端点：**
- `GET /api/v1/users/` 
- `GET /api/v1/users/groups/`
- `GET /api/v1/agents/`
- `GET /api/v1/tasks/`
- `GET /api/v1/alerts/rules/`
- `GET /api/v1/alerts/events/`
- `GET /api/v1/webhooks/`
- `GET /api/v1/webhooks/{id}/deliveries`

### 5.4 测试

- 更新所有 list 端点测试，验证返回 `items`/`total`/`skip`/`limit` 字段

---

## 实现顺序

1. **Feature 5 (分页)** — 先做，因为改动面最广但最机械，且后续 audit API 也需要分页
2. **Feature 1 (限流)** — 独立，简单
3. **Feature 2 (JWT 撤销)** — 需要改 auth 依赖签名，影响所有 router
4. **Feature 3 (审计日志)** — 依赖 Feature 2 的 actor_uuid 传递模式
5. **Feature 4 (Webhook 重试)** — 独立

---

## Verification

```bash
uv run ruff format . && uv run ruff check --fix .
uv run pytest --ignore=tests/functional/test_real_api.py --ignore=tests/core/test_config.py --ignore=tests/e2e/test_probe_pipeline.py -v
```
