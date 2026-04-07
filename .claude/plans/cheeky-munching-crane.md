# Plan: Database-Backed Settings with Admin API

## Context

All configuration is currently managed via `.env` file (`netpulse/core/config.py` `Settings` class). The user wants ALL settings to be viewable and configurable through admin API endpoints, eliminating the need to manually edit `.env` and restart.

**Key constraint**: Bootstrap settings (`DATABASE_URL`, `REDIS_URL`, etc.) are needed before the DB is available, so they must remain in `.env` as fallback. They'll be visible in the API (read-only) but not editable via API.

## Architecture

- **Key-value table** (`system_settings`) — one row per setting, flexible, no migration needed for new settings
- **Hybrid loading**: `.env` first (bootstrap), then DB overrides for runtime settings
- **Type safety**: JSON-encoded values with `value_type` discriminator column
- **Secret masking**: `is_secret` flag, API returns `"********"` for secret values
- **Admin-only**: All endpoints use `require_admin` pattern

## Files to Create

| File | Purpose |
|------|---------|
| `netpulse/modules/settings/__init__.py` | Empty module init |
| `netpulse/modules/settings/models.py` | `SystemSetting` SQLAlchemy model |
| `netpulse/modules/settings/schemas.py` | `SettingResponse`, `SettingUpdate`, `SettingBulkUpdate` |
| `netpulse/modules/settings/service.py` | CRUD + type validation + DB loading + cache |
| `netpulse/modules/settings/router.py` | Admin-only API endpoints |
| `alembic/versions/g7b8c9d0e1f2_add_system_settings.py` | Migration: create table + seed data |
| `tests/modules/settings/test_service.py` | Service unit tests |
| `tests/modules/settings/test_router.py` | Router functional tests |

## Files to Modify

| File | Change |
|------|--------|
| `alembic/env.py` | Add settings model import (line 24) |
| `netpulse/core/config.py` | Add `merge_db_settings()` function |
| `netpulse/main.py` | Load DB settings at startup, register settings router |
| `tests/functional/conftest.py` | Add settings router to test app |

## Implementation Steps

### Step 1: Model (`netpulse/modules/settings/models.py`)

```python
class SystemSetting(Base):
    __tablename__ = "system_settings"
    key: Mapped[str] = mapped_column(String(64), primary_key=True)  # e.g. "APP_NAME"
    value: Mapped[str] = mapped_column(Text)  # JSON-encoded: '"NetPulse"', '30', 'true'
    value_type: Mapped[str] = mapped_column(String(16))  # str/int/float/bool/json
    is_secret: Mapped[bool] = mapped_column(Boolean, default=False)
    is_bootstrap: Mapped[bool] = mapped_column(Boolean, default=False)
    description: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at / updated_at  # standard timestamps
```

Natural PK (`key`) instead of UUID — settings have well-known identifiers.

### Step 2: Migration

Chain after `f6a7b8c9d0e1`. Create table + seed all 22 settings with defaults, types, and flags.

**Bootstrap settings** (read-only via API): `DATABASE_URL`, `REDIS_URL`, `NATS_URL`, `NATS_TOKEN`, `VICTORIA_URL`, `JWT_SECRET`, `GEOLITE2_ASN_PATH`, `DEBUG`

**Secret settings** (masked in API): `JWT_SECRET`, `NATS_TOKEN`

### Step 3: Schemas (`netpulse/modules/settings/schemas.py`)

- `SettingResponse` — with `model_config = {"from_attributes": True}`, masks value when `is_secret=True`
- `SettingUpdate` — `value: str` (JSON-encoded), validated
- `SettingBulkUpdate` — `settings: list[SettingUpdateItem]` for batch updates

No `SettingCreate` — settings are seeded by migration only.

### Step 4: Service (`netpulse/modules/settings/service.py`)

**`SETTING_DEFINITIONS`** dict — single source of truth for all setting keys, types, defaults, flags.

Core functions:
- `list_settings(session, skip, limit)` → paginated list
- `get_setting(session, key)` → single setting or `NotFoundError`
- `update_setting(session, key, new_value, ...)` → validate type, reject bootstrap, audit log, invalidate cache
- `update_settings_bulk(session, items, ...)` → batch update
- `load_db_settings(session)` → `dict[str, Any]` with deserialized values (used at startup)
- `_validate_value(raw_json, value_type)` → type check helper
- `invalidate_settings_cache(redis_client)` → delete Redis key `system_settings:cache`

### Step 5: Router (`netpulse/modules/settings/router.py`)

Factory: `create_settings_router(secret, get_session, redis_client)` → `APIRouter`

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/v1/settings/` | List all (paginated) |
| GET | `/api/v1/settings/{key}` | Get one |
| PATCH | `/api/v1/settings/bulk` | Batch update (registered BEFORE `/{key}`) |
| PATCH | `/api/v1/settings/{key}` | Update one |

All admin-only via `require_admin`. Secret values masked in responses.

### Step 6: Config merge (`netpulse/core/config.py`)

Add `merge_db_settings(base: Settings, db_overrides: dict) -> Settings` — creates new Settings with DB values overriding non-bootstrap fields.

### Step 7: Main integration (`netpulse/main.py`)

After engine + session_factory creation, before Redis/VM/router setup:

```python
async def _load_db_settings():
    async with session_factory() as session:
        return await load_db_settings(session)

try:
    db_overrides = asyncio.run(_load_db_settings())
    settings = merge_db_settings(settings, db_overrides)
except Exception:
    logger.warning("Could not load DB settings, using .env defaults")
```

Safe because `create_app()` runs before the event loop starts. Gracefully falls back if table doesn't exist yet (pre-migration).

Register router:
```python
app.include_router(create_settings_router(secret=secret, get_session=get_session, redis_client=redis_client))
```

### Step 8: Tests

**Service tests** (`tests/modules/settings/test_service.py`):
- list/get/update CRUD operations
- Bootstrap rejection
- Type validation (str/int/float/bool/json)
- Secret masking in audit details

**Router tests** (`tests/modules/settings/test_router.py`):
- Admin can list/get/update
- Non-admin rejected (403)
- Secret values masked
- Bootstrap update rejected (400)
- Invalid type rejected (400)

**Conftest update**: Add settings router to functional test app.

## Important Notes

- **Restart required**: Runtime settings changed via API take effect after restart, because values are passed as primitives to router factories at startup. This is documented in API responses.
- **No psycopg2**: Project uses `asyncpg` only. Startup load uses `asyncio.run()` with async engine.
- **Graceful fallback**: If `system_settings` table doesn't exist (pre-migration), startup falls back to `.env` defaults silently.

## Verification

```bash
# Run migration
uv run alembic upgrade head

# Run tests
uv run pytest tests/modules/settings/ -v

# Manual test
curl -H "Authorization: Bearer <admin_token>" http://localhost:8000/api/v1/settings/
curl -X PATCH -H "Authorization: Bearer <admin_token>" -H "Content-Type: application/json" \
  -d '{"value": "120"}' http://localhost:8000/api/v1/settings/AGENT_HEARTBEAT_TTL
```
