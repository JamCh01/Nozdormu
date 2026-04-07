# Agent Version & Release Management Plan

## Context

The backend now returns `agent_version` and `platform` fields on agent responses, and has release management endpoints (list, upload, delete, push). The frontend needs to display these new fields and add a release management UI.

## Scope

1. Add version/platform columns to agent list table
2. Show version/platform on agent detail page
3. New release management tab/page with list, upload, delete, push
4. Extract `PLATFORM_OPTIONS` to shared location
5. i18n keys for both en and zh

## Key Findings

- `AgentResponse` already has `agent_version?: string | null` and `platform?: string | null`
- SDK functions already generated: `listReleasesApiV1AgentsReleasesGet`, `uploadReleaseApiV1AgentsReleasesUploadPost`, `deleteReleaseApiV1AgentsReleasesReleaseUuidDelete`, `pushUpdateApiV1AgentsReleasesReleaseUuidPushPost`
- `ReleaseListResponse` is NOT paginated — returns `{ releases: ReleaseResponse[] }`
- Upload uses `formDataBodySerializer` with `Content-Type: null` (multipart)
- `PLATFORM_OPTIONS` is local to `agents-page.tsx` — needs extraction
- Types: `ReleaseResponse`, `ReleaseListResponse`, `BodyUploadReleaseApiV1AgentsReleasesUploadPost`

---

## Step 1: Extract PLATFORM_OPTIONS to shared constants

**File:** `src/lib/constants.ts`

Move `PLATFORM_OPTIONS` from `agents-page.tsx` to `src/lib/constants.ts` so both agents-page and the new releases page can import it.

```ts
export const PLATFORM_OPTIONS = [
  { value: 'x86_64-linux-musl', labelKey: 'agents.platformLinuxAmd64' },
  { value: 'aarch64-linux-musl', labelKey: 'agents.platformLinuxArm64' },
  { value: 'x86_64-macos', labelKey: 'agents.platformDarwinAmd64' },
  { value: 'aarch64-macos', labelKey: 'agents.platformDarwinArm64' },
  { value: 'x86_64-windows', labelKey: 'agents.platformWindowsAmd64' },
  { value: 'aarch64-windows', labelKey: 'agents.platformWindowsArm64' },
] as const
```

Update `agents-page.tsx` to import from `@/lib/constants`.

---

## Step 2: Add version/platform to agent list and detail

### `src/features/agents/pages/agents-page.tsx`
- Add two table columns after "Tags": **Version** (`agent_version ?? '-'`) and **Platform** (resolved via `PLATFORM_OPTIONS` label or raw value)
- Version column: monospace, small text
- Platform column: small badge-style text

### `src/features/agents/pages/agent-detail-page.tsx`
- Add version and platform to the Agent Info card (alongside UUID, name, created_at)
- Show `agent_version ?? t('common.na')` and platform label

---

## Step 3: Release hooks and query keys

### `src/api/hooks/keys.ts`
Add:
```ts
export const releaseKeys = {
  all: ['releases'] as const,
  list: (platform?: string | null) => [...releaseKeys.all, 'list', platform] as const,
  detail: (uuid: string) => [...releaseKeys.all, 'detail', uuid] as const,
}
```

### New `src/api/hooks/use-releases.ts`
- `useReleases(platform?)` — query, calls `listReleasesApiV1AgentsReleasesGet`, returns `ReleaseListResponse`
- `useUploadRelease()` — mutation, calls `uploadReleaseApiV1AgentsReleasesUploadPost`, invalidates `releaseKeys.all`
- `useDeleteRelease()` — mutation, calls `deleteReleaseApiV1AgentsReleasesReleaseUuidDelete`, invalidates `releaseKeys.all`
- `usePushUpdate()` — mutation, calls `pushUpdateApiV1AgentsReleasesReleaseUuidPushPost` (no cache invalidation needed, returns `{ pushed: number }`)

---

## Step 4: Release management page

### New `src/features/agents/pages/releases-page.tsx`

**Layout:**
- Title: "Release Management" with back link to `/agents`
- Platform filter: Select dropdown (all platforms + each platform option)
- Release table grouped by platform, sorted by version desc
- "Upload Release" button opens upload dialog

**Table columns:** Version, Platform, Filename, File Size (formatted), SHA256 (truncated + copy), Release Notes, Latest (badge), Created At, Actions

**Actions per row:**
- Push Update — opens confirmation dialog showing "Push v{version} to N {platform} agents?"
- Delete — confirmation dialog

**Upload dialog:**
- File input (required)
- Version input (required, e.g. "1.2.0")
- Platform select (required, from PLATFORM_OPTIONS)
- Release notes textarea (optional)
- Submit calls `useUploadRelease()`

**Push confirmation dialog:**
- Shows version + platform
- On confirm, calls `usePushUpdate()`, shows result `{ pushed: N }` as success message

### Route
Add `/agents/releases` inside AdminGuard in `src/router.tsx`.

### Navigation
Add a "Release Management" button/link on the agents page header (next to "Create Agent").

---

## Step 5: i18n

Add to `en.json` and `zh.json` in the `agents` section:

```
"version": "Version",
"releases": "Release Management",
"releasesDesc": "Manage agent binary releases across platforms.",
"uploadRelease": "Upload Release",
"uploadReleaseDesc": "Upload a new agent binary release.",
"versionLabel": "Version",
"versionPlaceholder": "e.g. 1.2.0",
"releaseNotes": "Release Notes",
"releaseNotesPlaceholder": "Optional release notes",
"file": "File",
"fileSize": "File Size",
"sha256": "SHA256",
"latest": "Latest",
"pushUpdate": "Push Update",
"pushConfirm": "Push v{{version}} to all {{platform}} agents?",
"pushSuccess": "Successfully pushed to {{count}} agents.",
"deleteRelease": "Delete Release",
"deleteReleaseConfirm": "Are you sure you want to delete v{{version}} for {{platform}}?",
"uploadFailed": "Failed to upload release.",
"noReleases": "No releases uploaded yet.",
"allPlatforms": "All Platforms",
"filename": "Filename"
```

---

## Step 6: Mock handlers and factories

### `src/test/mocks/data/factories.ts`
Add `createMockRelease()` factory.

### New `src/test/mocks/handlers/releases.ts`
- `GET */api/v1/agents/releases/` → `{ releases: [release1, release2] }`
- `POST */api/v1/agents/releases/upload` → `createMockRelease()` (201)
- `DELETE */api/v1/agents/releases/:releaseUuid` → 204
- `POST */api/v1/agents/releases/:releaseUuid/push` → `{ pushed: 5 }`

Register in `src/test/mocks/server.ts`.

---

## File Summary

| Action | File |
|--------|------|
| Modify | `src/lib/constants.ts` — add PLATFORM_OPTIONS |
| Modify | `src/features/agents/pages/agents-page.tsx` — import PLATFORM_OPTIONS, add version/platform columns |
| Modify | `src/features/agents/pages/agent-detail-page.tsx` — show version/platform |
| Modify | `src/api/hooks/keys.ts` — add releaseKeys |
| Create | `src/api/hooks/use-releases.ts` |
| Create | `src/features/agents/pages/releases-page.tsx` |
| Modify | `src/router.tsx` — add /agents/releases route |
| Modify | `src/i18n/locales/en.json` — add release keys |
| Modify | `src/i18n/locales/zh.json` — add release keys |
| Create | `src/test/mocks/handlers/releases.ts` |
| Modify | `src/test/mocks/data/factories.ts` — add createMockRelease |
| Modify | `src/test/mocks/server.ts` — register releaseHandlers |

---

## Verification

1. `npx tsc -b` — no type errors
2. `npm run test` — all tests pass
3. `npm run lint` — no new errors (generated files excluded)
4. Manual: agent list shows version + platform columns
5. Manual: agent detail shows version + platform
6. Manual: `/agents/releases` page loads, filters by platform
7. Manual: upload dialog works with file + version + platform
8. Manual: push update shows confirmation + result count
9. Manual: delete release with confirmation
