# Monitor Center P0 Backend - Implementation Plan

## Context

ISP/IDC monitoring system center-side backend. Greenfield project in `C:\Users\user\Desktop\monitor`. Goal: build the complete P0 backend with FastAPI + async PostgreSQL + ClickHouse + Kafka, covering MQ consumption, alert engine, incident lifecycle, notification, maintenance, on-call, integration center, and all REST APIs defined in FEATURE.md.

**Tech stack**: Python 3.12, uv, FastAPI, asyncpg + SQLAlchemy 2.0 async, asynch (ClickHouse), aiokafka, Alembic, Pydantic v2, structlog.

---

## Project Structure

```
monitor/
├── pyproject.toml
├── .python-version                    # 3.12
├── .env.example
├── alembic.ini
├── migrations/
│   ├── env.py
│   ├── script.py.mako
│   └── versions/                      # 8 migration files
├── clickhouse/
│   ├── 001_metric_raw.sql ... 005_event_log_raw.sql
│   └── apply.py
├── src/monitor/
│   ├── main.py                        # FastAPI app factory + lifespan
│   ├── settings.py                    # pydantic-settings
│   ├── common/                        # database.py, clickhouse.py, kafka.py, errors.py, envelopes.py, pagination.py, schemas.py, dependencies.py, logging.py, uuidv7.py
│   ├── models/                        # SQLAlchemy ORM: base.py, agent.py, target.py, rule.py, alert.py, incident.py, event.py, maintenance.py, notification.py, audit.py, oncall.py, traffic.py
│   ├── repositories/                  # base.py + 11 domain repos
│   ├── services/                      # normalizer, ch_writer, pg_metadata, rule_engine, alert_manager, incident_manager, escalation, impact_engine, notifier, maintenance, oncall, integration, dcim_sync, permission
│   ├── consumers/                     # base.py + 8 consumers + registry.py
│   ├── schedulers/                    # base.py + 3 schedulers + registry.py
│   ├── api/                           # router.py, deps.py + 9 sub-packages (agent/, ingest/, integration/, targets/, alerts/, incidents/, rules/, maintenance/, oncall/)
│   └── notifications/                 # base.py, wecom.py, phone.py
├── tests/
└── docker/
    └── docker-compose.dev.yml
```

---

## Implementation Phases (8 phases)

### Phase 1: Foundation & Project Skeleton
- `pyproject.toml` with all deps, `uv` setup, `.python-version`, `.env.example`
- `src/monitor/settings.py` (pydantic-settings)
- `src/monitor/common/` all modules (database, clickhouse, kafka, errors, envelopes, pagination, schemas, dependencies, logging, uuidv7)
- `src/monitor/models/base.py` + `models/agent.py` (AgentNode, AgentProfile, AgentProfileVersion, AgentProfileBinding, AgentTask, AgentTaskRun)
- `alembic.ini` + `migrations/env.py` + first migration
- ClickHouse DDLs (metric_raw, rollup MVs, event_log_raw) + apply script
- `src/monitor/main.py` (lifespan: PG engine, CH pool, Kafka producer)
- `src/monitor/consumers/base.py` (BaseConsumer with DLQ)
- `src/monitor/consumers/heartbeat_consumer.py`
- `docker/docker-compose.dev.yml` (PG + CH + Kafka + ZK)

### Phase 2: Data Ingest Pipeline
- `models/target.py`, `models/event.py` + migrations
- `repositories/base.py`, `repositories/target_repo.py`
- `services/normalizer.py` (B-01)
- `services/ch_writer.py` (B-02) - batched async writes
- `services/pg_metadata.py` (B-03) - target upsert, event index
- `consumers/metrics_consumer.py`, `consumers/events_consumer.py`
- `consumers/registry.py` - wire into lifespan

### Phase 3: Alert Rule Engine
- `models/rule.py`, `models/alert.py`, `models/maintenance.py` + migrations
- `repositories/rule_repo.py`, `repositories/alert_repo.py`, `repositories/maintenance_repo.py`
- `services/rule_engine.py` (B-04) - THRESHOLD/NO_DATA/anti-flap evaluators
- `schedulers/base.py` + `schedulers/rule_eval_scheduler.py`
- `services/alert_manager.py` (B-05) - dedup, storm, FLAPPING
- `consumers/alert_state_consumer.py`
- `services/maintenance.py` (B-10) - window matching, recurrence

### Phase 4: Incident Lifecycle + Escalation
- `models/incident.py`, `models/oncall.py` + migrations
- `repositories/incident_repo.py`, `repositories/oncall_repo.py`
- `services/incident_manager.py` (B-06) - state machine
- `services/escalation.py` (B-07) - SLA checker
- `schedulers/escalation_scheduler.py`, `schedulers/maintenance_scheduler.py`
- `services/oncall.py` (B-11) - shift resolution

### Phase 5: Notification + Impact
- `models/notification.py` + migration
- `notifications/base.py`, `notifications/wecom.py`, `notifications/phone.py`
- `services/notifier.py` (B-09)
- `services/impact_engine.py` (B-08) - mock graph service
- `consumers/notify_request_consumer.py`, `consumers/impact_request_consumer.py`, `consumers/impact_result_consumer.py`

### Phase 6: REST APIs (all endpoints from FEATURE.md 7.1-7.10)
- `api/deps.py`, `api/router.py`
- `api/agent/` (7.1 + 7.9), `api/ingest/` (7.2), `api/targets/` (7.4)
- `api/alerts/` (7.5), `api/incidents/` (7.6), `api/rules/` (7.7)
- `api/maintenance/` (7.8), `api/oncall/` (7.10), `api/integration/` (7.3)

### Phase 7: DCIM Sync + Permissions + Remaining
- `services/dcim_sync.py` (B-13) + `consumers/dcim_consumer.py`
- `services/permission.py` (B-14) - customer isolation, public infra union
- `services/integration.py` (B-12) - token, mapping, health, DLQ
- `models/audit.py`, `models/traffic.py` + migrations
- `repositories/audit_repo.py`, `repositories/traffic_repo.py`

### Phase 8: Health Check + Final Wiring
- `/health` endpoint reporting all subsystem status
- Scheduler/consumer registries finalized
- All migrations consolidated and verified

---

## Key Design Decisions

1. **Layered architecture**: API routes (thin) -> Services (business logic) -> Repositories (data access)
2. **Background workers**: Kafka consumers + periodic schedulers run as `asyncio.Task` in FastAPI lifespan, sharing the event loop
3. **Session management**: API routes use `Depends(get_db_session)`; background workers create sessions per-iteration from `app.state.session_factory`
4. **Idempotency**: ClickHouse `ReplacingMergeTree` for metrics; PG `ON CONFLICT DO NOTHING` for events; `dedup_key` for alerts
5. **DLQ**: Base consumer catches exceptions and publishes to `dlq.{topic}`, always commits offset
6. **ClickHouse query routing**: `resolve_ch_table(start, end)` enforces the 48h/90d/365d tier rules
7. **State machine**: Incident transitions via declarative table `{(current, action): next_state}`
8. **Anti-flap**: Recovery requires value below `threshold - hysteresis` for `recovery_duration`
9. **Graceful shutdown**: Consumers use `_running` flag; schedulers use `asyncio.Event`

---

## Verification

1. `uv run alembic upgrade head` - all PG migrations apply cleanly
2. `python clickhouse/apply.py` - all CH DDLs apply
3. `uv run uvicorn monitor.main:app` - app starts, connects to PG/CH/Kafka, consumers and schedulers launch
4. `GET /health` - returns status of all subsystems
5. POST to `/api/ingest/metrics` and `/api/ingest/events` - data flows through Kafka -> CH + PG
6. All API endpoints respond with correct schemas (test via OpenAPI docs at `/docs`)
