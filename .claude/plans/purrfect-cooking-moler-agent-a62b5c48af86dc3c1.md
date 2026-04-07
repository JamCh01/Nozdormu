
# MTR (My Traceroute) Task Type - Implementation Plan

## Summary

Add mtr as a 5th probe protocol to NetPulse. MTR produces per-hop data rather than a single endpoint measurement. MTR probe results flow through the same NATS subject (netpulse.probes.data), but the worker writes hop-level metrics to VictoriaMetrics with new mtr_hop_* metric names. A new monitoring endpoint serves hop-by-hop query results.

---

## Phase 1: Database Migration

Goal: Add mtr to the PostgreSQL probeprotocol enum and add 4 new nullable columns to the tasks table.

### File: alembic/versions/NEW_add_mtr_protocol_and_fields.py

Create a new Alembic migration (revision depends on 4ceb6d9025a6).

upgrade():
1. ALTER TYPE probeprotocol ADD VALUE MTR via op.execute raw SQL. PostgreSQL enum ADD VALUE cannot run inside a transaction block. Use COMMIT/BEGIN wrapper or Alembic 1.12+ autocommit_block.
2. op.add_column tasks max_hops Integer nullable=True
3. op.add_column tasks loss_threshold Float nullable=True
4. op.add_column tasks cooldown_secs Integer nullable=True
5. op.add_column tasks max_retries Integer nullable=True

downgrade(): Drop all 4 columns. Enum value removal not possible in PG (safe to leave).

Key: All 4 columns nullable. No backfill needed.

---

## Phase 2: Task Model

### File: netpulse/modules/tasks/models.py

1. Add MTR = "mtr" to ProbeProtocol enum
2. Add 4 nullable columns to Task model (after timeout, before is_active):
   max_hops (Integer), loss_threshold (Float), cooldown_secs (Integer), max_retries (Integer)
   Import Float from sqlalchemy.

---

## Phase 3: Task Schemas

### File: netpulse/modules/tasks/schemas.py

1. Add MTR = "mtr" to ProtocolEnum
2. Update TaskCreate: add 4 optional MTR fields. Expand model_validator:
   - protocol==mtr: apply defaults (max_hops=30, loss_threshold=10.0, cooldown_secs=300, max_retries=3), override interval to 1800 if not explicitly set (use self.model_fields_set)
   - protocol!=mtr: null out MTR fields silently. Keep port validation for tcp/http.
3. Update TaskResponse: add 4 nullable fields
4. Update TaskUpdate: add 4 optional MTR fields
5. Update TaskDispatch: add 4 optional MTR fields

---

## Phase 4: Task Service

### File: netpulse/modules/tasks/service.py

1. Update create_task: pass 4 MTR fields to Task constructor
2. Update update_task ALLOWED_FIELDS: add max_hops, loss_threshold, cooldown_secs, max_retries

---

## Phase 5: Task Router (NATS Dispatch)

### File: netpulse/modules/tasks/router.py

Update _notify_agents TaskDispatch construction to include 4 MTR fields.

---

## Phase 6: Worker Probe Consumer (MTR Metric Ingestion)

### File: netpulse/worker/consumers/probe_consumer.py

Most significant change.

1. Add MTR_HOP_METRIC_KEYS = ["avg_rtt", "min_rtt", "max_rtt", "packet_loss_pct"]
2. Add convert_mtr_to_metric_lines(payload): iterate hops, emit mtr_hop_* lines with hop/hop_ip labels, plus mtr_target_reached and mtr_total_hops summary metrics
3. Update handle_probe_message: branch on parsed.get("protocol") == "mtr"
4. Update parse_probe_message: validate results.hops for MTR messages

---

## Phase 7: MTR Monitoring Endpoint

### File: netpulse/modules/monitoring/schemas.py
Add: MtrMonitoringQuery, MtrHopDataPoint, MtrDataPoint, MtrMonitoringResponse

### File: netpulse/modules/monitoring/service.py
Add query_mtr_data(vm_client, query): query VM for mtr_hop_* metrics, merge by timestamp/hop, return MtrMonitoringResponse

### File: netpulse/modules/monitoring/router.py
Add POST /api/v1/monitoring/mtr endpoint (VM-only, no session/Redis)

---

## Phase 8-9: Aggregation and Alert Evaluation - No Changes

Existing jobs use probe_* prefix. MTR uses mtr_hop_* prefix. Naturally excluded. Deferred.

---

## Phase 10: Tests

### 10.1 Model: test_models.py - enum, column existence, nullable checks
### 10.2 Schema: test_schemas.py - MTR defaults, interval override, port not required, field nulling for non-MTR
### 10.3 Service: test_service.py - create/update MTR tasks
### 10.4 Router: test_router.py - HTTP create MTR task
### 10.5 Probe Consumer: test_probe_consumer.py - MTR metric conversion, hop labels, summary metrics, buffer integration
### 10.6 Monitoring: test_schemas.py, test_service.py, test_router.py - MTR query schemas and endpoint
### 10.7 E2E: test_probe_pipeline.py - full MTR pipeline
### 10.8 Functional: test_task_management.py, conftest.py - MTR task CRUD

---

## Phase 11: Documentation - CLAUDE.md updates

---

## TDD Order: model -> schema -> service -> router -> consumer -> monitoring -> e2e -> migration -> docs

## Risks: PG enum transaction (COMMIT/BEGIN wrapper), backward compat (protocol field optional), data volume (1800s default interval), cardinality (monitor), interval default (model_fields_set)
