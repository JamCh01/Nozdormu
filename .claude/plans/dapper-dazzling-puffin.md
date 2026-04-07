# Plan: Adapt Frontend to Latest Backend API

## Context

The backend API at `127.0.0.1:8000` has been updated. The frontend's `docs/openapi.json` is stale (Apr 6 04:48) vs the backend's (Apr 6 14:25). We need to:
1. Copy the latest `openapi.json` from the backend
2. Regenerate the API client
3. Fix all type/hook/page breakages caused by the schema changes

## Exact Differences (backend new vs frontend old)

### New schemas
- `AuditLogResponse` - typed audit log with `log_uuid`, `actor_uuid`, `actor_role`, `action`, `resource_type`, `resource_uuid`, `details`, `ip_address`, `created_at`
- `PaginatedResponse_AuditLogResponse_` - paginated wrapper

### Schema field additions
- `HeartbeatRequest`: + `access_key` (required) -- **no frontend impact** (only agents call heartbeat)
- `MtrResultListResponse`: + `total`, `skip`, `limit` fields (pagination support)

### Endpoint changes
- `GET /api/v1/audit/logs` [200]: response changed from `unknown` to `PaginatedResponse[AuditLogResponse]`
- `GET /api/v1/audit/logs`: removed `authorization` header param (auth still works via interceptor)
- `GET /api/v1/monitoring/mtr`: + `skip`, `limit` query params
- `GET /api/v1/agents/{agent_uuid}/tasks`: + `X-Access-Key` header param (agent-facing, frontend uses JWT -- no impact)

## Implementation Steps

### Step 1: Copy latest openapi.json & regenerate
- Copy `NetPulseAPI/docs/openapi.json` to `NetPulseFrontend/docs/openapi.json`
- Update `openapi-ts.config.ts` input path if needed (currently `../docs/openapi.json`)
- Run `npm run generate:api` in `frontend/`
- This will regenerate `types.gen.ts` and `sdk.gen.ts` with all the new types

### Step 2: Fix `use-mtr.ts` hook -- add skip/limit params
- **File:** `frontend/src/api/hooks/use-mtr.ts`
- Add optional `skip`/`limit` params to `useMtrList`
- Pass them through to the SDK query
- Update query key to include skip/limit

### Step 3: Fix `use-audit.ts` hook -- add return type
- **File:** `frontend/src/api/hooks/use-audit.ts`
- The hook already passes `skip`/`limit`/filters correctly
- After regeneration, the return type will be `PaginatedResponse[AuditLogResponse]` instead of `unknown`
- No code changes needed in the hook itself -- the generated SDK will return the correct type

### Step 4: Fix audit page -- remove local `AuditLog` interface
- **File:** `frontend/src/features/audit/pages/audit-page.tsx`
- Remove the local `AuditLog` interface (lines 22-32)
- Import `AuditLogResponse` from `@/api/generated/types.gen`
- Remove `as` casts on `paginatedData` -- the data will be properly typed
- Replace `AuditLog` references with `AuditLogResponse`

### Step 5: Verify MTR detail page still works
- **File:** `frontend/src/features/monitoring/pages/mtr-detail-page.tsx`
- `MtrResultListResponse` now has `total`/`skip`/`limit` fields
- The page accesses `mtrListData?.results` which still exists -- no breakage
- No pagination UI needed (the page shows scatter chart of all results in time range)
- The hook defaults to no skip/limit, so all results are returned as before

### Step 6: Verify no TypeScript build errors
- Run `npm run build` to check for type errors
- Fix any remaining type issues from the regeneration

## Files to modify
1. `docs/openapi.json` -- replace with backend version
2. `frontend/src/api/generated/types.gen.ts` -- auto-regenerated
3. `frontend/src/api/generated/sdk.gen.ts` -- auto-regenerated
4. `frontend/src/api/hooks/use-mtr.ts` -- add skip/limit params
5. `frontend/src/features/audit/pages/audit-page.tsx` -- use generated `AuditLogResponse` type

## Files NOT modified (no impact)
- Agent hooks/pages -- still return `unknown` (backend spec unchanged for agents)
- Dashboard/health hooks -- still return `unknown` (backend spec unchanged)
- Webhook pages -- `next_retry_at` not in backend spec, existing `as` casts remain
- HeartbeatRequest -- only used by agents, not frontend

## Verification
1. `npm run generate:api` succeeds
2. `npm run build` succeeds with no type errors
3. `npm run dev` -- test against running backend at 127.0.0.1:8000:
   - Login works
   - Audit logs page loads with proper typing
   - MTR detail page loads
   - Monitoring pages work
