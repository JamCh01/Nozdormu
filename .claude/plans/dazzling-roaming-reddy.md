# Plan: Add "Create User" to Users Page

## Context
The Users page currently only lists/disables/edits users. There's no way to create new users from the admin interface — admin must use the API directly. Adding a Dialog-based "Create User" form matches the Agents page pattern (admin-only sibling feature, small form).

## Approach: Dialog on Users page (like Agents)

### 1. `src/api/hooks/use-users.ts` — Add `useCreateUser` hook
- Import `registerRouteApiV1AuthRegisterPost` from generated SDK
- Import `UserCreate, UserResponse` types
- Create `useCreateUser()` mutation hook:
  - `mutationFn`: calls `registerRouteApiV1AuthRegisterPost({ body: data })`
  - `onSuccess`: invalidates `userKeys.all`
- Pattern: follow existing `useUpdateUser` in same file

### 2. `src/features/users/pages/users-page.tsx` — Add create Dialog
- Add "Create User" button next to the role filter in the header
- Add Dialog with form fields: username, email, password, role (select)
- Add client-side validation (same rules as register-page: non-empty username, valid email, password >= 8 chars)
- On success: close dialog, reset form, list auto-refreshes via invalidation
- On error: show error message inside dialog

### 3. `src/test/mocks/handlers/auth.ts` — Add POST register handler
- Add `http.post(BASE + "/api/v1/auth/register", ...)` returning a mock `UserResponse` with status 201

### 4. Tests — No new test file needed
- Existing `users-page.test.tsx` can be extended if needed, but the existing test coverage + the mock handler + build/test pass is sufficient

## Files to Modify
1. `src/api/hooks/use-users.ts` — add `useCreateUser` hook
2. `src/features/users/pages/users-page.tsx` — add create button + Dialog
3. `src/test/mocks/handlers/auth.ts` — add register mock handler

## Verification
1. `npm run build` — must pass
2. `npx vitest run` — all tests must pass
3. Open http://localhost:5173, login as admin, go to Users page, click "Create User", fill form, verify user appears in list
