# Monitoring Module Restructuring Plan

## Context

The current monitoring data model doesn't match the actual use case. The platform monitors network latency from Agent probes to multiple target hosts. The current design has a single `target_host` per task and a separate `TaskConfig` table with `agent_ids[]` — this is backwards. The new design:

- **MonitoringTask** binds to one Agent (the source probe), contains multiple targets
- **MonitoringTarget** (new) is a child of Task — each target has its own host, protocol (icmp/tcp), and config params
- **TaskConfig** is deleted entirely
- **AlertRule/AlertEvent** now reference `target_id` instead of `task_id`
- **merchant_id/sku_id** removed from MonitoringTask

## Blast Radius: 16 files

### Files to REWRITE (core changes):
1. `app/models/monitoring.py` — replace MonitoringTask + delete TaskConfig + add MonitoringTarget
2. `app/schemas/monitoring.py` — new Task/Target schemas
3. `app/services/monitoring_service.py` — new Task/Target CRUD
4. `app/api/v1/monitoring.py` — new endpoints
5. `app/models/alerting.py` — task_id → target_id in AlertRule + AlertEvent
6. `tests/test_models_monitoring.py` — rewrite
7. `tests/test_schemas_monitoring.py` — rewrite
8. `tests/test_service_monitoring.py` — rewrite

### Files to UPDATE (minor changes):
9. `app/models/__init__.py` — replace TaskConfig with MonitoringTarget
10. `app/schemas/alerting.py` — task_id → target_id in AlertRuleCreate/Read
11. `app/services/alerting_service.py` — get_task → get_target
12. `app/workers/probe_consumer.py` — update labels: task_id → target_id
13. `app/workers/alert_evaluator.py` — rule.task_id → rule.target_id
14. `app/services/tsdb_service.py` — task_id → target_id in PromQL labels
15. `app/api/v1/tsdb.py` — task_id → target_id in path params
16. `tests/test_models_alerting.py` — update column assertions
17. `tests/test_schemas_alerting.py` — update field names
18. `tests/test_service_alerting.py` — update field names
19. `tests/test_service_tsdb.py` — update label assertions
20. `tests/test_worker_probe.py` — update label assertions

### Files UNCHANGED:
- `app/models/base.py`, `app/models/user.py`, `app/models/merchant.py`, `app/models/sku.py`
- `app/services/agent_service.py`, `app/api/v1/agents.py` (Agent model unchanged)
- `app/api/v1/router.py` (same router names)

### Alembic:
- New migration needed (drop task_configs, alter monitoring_tasks, create monitoring_targets, alter alert_rules/alert_events)

---

## New Data Model

### monitoring_tasks (modified)
```sql
CREATE TABLE monitoring_tasks (
    id          UUID PRIMARY KEY,
    agent_id    UUID NOT NULL REFERENCES agents(id),
    name        VARCHAR(300) NOT NULL,
    is_active   BOOLEAN NOT NULL DEFAULT true,
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_tasks_agent ON monitoring_tasks(agent_id);
```

### monitoring_targets (new, replaces task_configs)
```sql
CREATE TABLE monitoring_targets (
    id          UUID PRIMARY KEY,
    task_id     UUID NOT NULL REFERENCES monitoring_tasks(id) ON DELETE CASCADE,
    name        VARCHAR(300),
    target_host VARCHAR(500) NOT NULL,
    protocol    VARCHAR(10) NOT NULL DEFAULT 'icmp',  -- 'icmp' | 'tcp'
    port        INTEGER,                               -- TCP only
    packet_size INTEGER NOT NULL DEFAULT 64,
    interval    INTEGER NOT NULL DEFAULT 10,           -- packets per minute
    timeout     INTEGER NOT NULL DEFAULT 2,            -- seconds
    is_active   BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_targets_task ON monitoring_targets(task_id);
```

### alert_rules (modified)
```sql
-- task_id → target_id
ALTER TABLE alert_rules RENAME COLUMN task_id TO target_id;
-- FK now references monitoring_targets(id)
```

### alert_events (modified)
```sql
-- task_id → target_id
ALTER TABLE alert_events RENAME COLUMN task_id TO target_id;
```

### task_configs (dropped)
```sql
DROP TABLE task_configs;
```

---

## New API Shape

### Task endpoints
| Method | Path | Body |
|--------|------|------|
| POST | `/monitoring/tasks` | `{agent_id, name, targets: [{name, target_host, protocol, port, packet_size, interval, timeout}]}` |
| GET | `/monitoring/tasks` | paginated list |
| GET | `/monitoring/tasks/{id}` | detail with targets |
| PATCH | `/monitoring/tasks/{id}` | `{name, is_active}` |
| DELETE | `/monitoring/tasks/{id}` | cascade deletes targets |

### Target endpoints (nested)
| Method | Path | Body |
|--------|------|------|
| POST | `/monitoring/tasks/{id}/targets` | `{name, target_host, protocol, ...}` |
| PATCH | `/monitoring/tasks/{id}/targets/{tid}` | partial update |
| DELETE | `/monitoring/tasks/{id}/targets/{tid}` | delete single target |

### Alerting changes
- `POST /alerting/rules` body: `target_id` instead of `task_id`
- `GET /alerting/events` response: `target_id` instead of `task_id`

### TSDB changes
- `/tsdb/targets/{target_id}/summary` instead of `/tsdb/tasks/{task_id}/summary`
- `/tsdb/targets/{target_id}/chart` instead of `/tsdb/tasks/{task_id}/chart`

---

## Implementation Steps

### Step 1: Rewrite models/monitoring.py

Remove `TaskConfig`. Modify `MonitoringTask` (drop merchant_id, sku_id, target_host; add agent_id FK to agents). Add `MonitoringTarget` with all config fields as columns.

### Step 2: Update models/alerting.py

Rename `task_id` → `target_id` in AlertRule and AlertEvent. Update FK to `monitoring_targets.id`.

### Step 3: Update models/__init__.py

Replace `TaskConfig` export with `MonitoringTarget`.

### Step 4: Rewrite schemas/monitoring.py

New schemas: `MonitoringTaskCreate` (with embedded `targets`), `MonitoringTargetCreate/Read/Update`, `MonitoringTaskRead` (with `targets` list and `agent_id`). Remove all TaskConfig schemas.

### Step 5: Update schemas/alerting.py

`task_id` → `target_id` in AlertRuleCreate, AlertRuleRead, AlertEventRead.

### Step 6: Rewrite services/monitoring_service.py

New CRUD for Task (with targets creation) and Target. Remove all TaskConfig functions.

### Step 7: Update services/alerting_service.py

`get_task` → `get_target` validation. `data.task_id` → `data.target_id`.

### Step 8: Rewrite api/v1/monitoring.py

New endpoints matching the API shape above. Remove TaskConfig endpoints.

### Step 9: Update services/tsdb_service.py

`task_id` → `target_id` in PromQL label building.

### Step 10: Update api/v1/tsdb.py

Path params: `task_id` → `target_id`.

### Step 11: Update workers/probe_consumer.py

Labels: `task_id` → `target_id`.

### Step 12: Update workers/alert_evaluator.py

`rule.task_id` → `rule.target_id`.

### Step 13: Rewrite all monitoring tests

### Step 14: Update alerting + tsdb + worker tests

### Step 15: Generate Alembic migration

### Step 16: Run all tests + lint

---

## Verification

1. `uv run pytest -v` — all tests pass
2. `uv run ruff check app/ tests/` — clean
3. App starts without import errors
4. OpenAPI docs show new task/target structure
5. Alembic migration applies cleanly on server
